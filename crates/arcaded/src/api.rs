//! Local API: one JSON request per line on a Unix socket, one JSON answer per line.
//! Used by the `arcade` CLI and the desktop app.

use crate::node::Node;
use arcade_local::{UnixListener, UnixStream};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;

/// The API socket, bound (a stale one from an earlier run removed first). Panics when it can't be: the arcade is no use
/// without it.
pub fn bind(path: &std::path::Path) -> UnixListener {
    if let Err(e) = arcade_local::check_socket_path(path) {
        panic!("bind API socket: {e}");
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path).unwrap_or_else(|e| panic!("bind API socket {}: {e}", path.display()));
    // Group-readable on Linux (the service user and the arcade group). On Windows the
    // socket lives in the user's own local app data, which other users cannot read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660));
    }
    l
}

pub fn serve_on(node: Arc<Node>, l: UnixListener) {
    for s in l.incoming().flatten() {
        let node = node.clone();
        std::thread::spawn(move || handle(node, s));
    }
}

/// Whether arcaded has finished starting (its chains open, its services running). The socket opens before that (main:
/// opening the chains can take minutes on a slow disk), so the desktop app and the CLI reach a starting arcade and say
/// so, instead of "not running" (on Windows the app then tried to start a second one).
static READY: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());
/// How long a request that arrives while arcaded is starting waits for it before it is answered "starting".
const STARTING_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// arcaded has started: every request is answered from now on.
pub fn set_ready() {
    *READY.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
    READY.1.notify_all();
}

/// Wait up to `limit` for arcaded to finish starting; whether it has.
fn wait_ready(limit: std::time::Duration) -> bool {
    let g = READY.0.lock().unwrap_or_else(|p| p.into_inner());
    let (g, _) = READY.1.wait_timeout_while(g, limit, |ready| !*ready).unwrap_or_else(|p| p.into_inner());
    *g
}

/// The answer to a request that came while arcaded is still starting (`starting`, as the desktop app's own answer when
/// it starts the service: the window says "starting the service").
pub fn starting_reply() -> Value {
    json!({"ok": false, "error": "the arcade service is starting (opening its chains)", "starting": true, "state": "starting", "code": "starting", "retry_after_s": 5})
}

/// Where arcaded is in its life: "starting" (until set_ready), "ready", or "stopping" (once a stop began: writing the
/// coin caches, then exiting). ARCADE #25: a stop of several minutes looked like a crash.
pub fn phase() -> &'static str {
    if crate::STOP_ASKED.load(std::sync::atomic::Ordering::SeqCst) {
        "stopping"
    } else if !wait_ready(std::time::Duration::ZERO) {
        "starting"
    } else {
        "ready"
    }
}

/// The answer to a request that came after a stop began.
pub fn stopping_reply() -> Value {
    json!({"ok": false, "error": "the arcade service is stopping (writing the coin caches, then exiting)", "stopping": true, "state": "stopping", "code": "stopping", "retry_after_s": 10})
}

/// Every error code an answer may carry (openapi.json lists them; agents match on these, not on the text).
pub const ERROR_CODES: &[&str] = &[
    "unknown_command", "unknown_route", "not_found", "bad_args", "forbidden", "unknown_chain", "not_installed",
    "insufficient_funds", "not_enough_arcade", "client_ref_used", "starting", "stopping", "error",
];

/// A stable code for an error's text (ARCADE #25: scripts matched the text): kept beside `error`.
pub fn error_code(e: &str) -> &'static str {
    let l = e.to_ascii_lowercase();
    if l.starts_with("client_ref ") && l.contains("already signed") {
        "client_ref_used"
    } else if l.starts_with("unknown command") || l.starts_with("unknown method") {
        "unknown_command"
    } else if l.starts_with("unknown route") {
        "unknown_route"
    } else if l.contains("not enough arcade") {
        "not_enough_arcade"
    } else if l.contains("not enough") || l.contains("insufficient") {
        "insufficient_funds"
    } else if l.starts_with("unknown chain") {
        "unknown_chain"
    } else if l.starts_with("not_found") || l.starts_with("no transaction") || l.starts_with("no block") || l.starts_with("no such") || l.contains(" not found") {
        "not_found"
    } else if l.contains("service is starting") || l.starts_with("starting:") {
        "starting"
    } else if l.contains("service is stopping") {
        "stopping"
    } else if l.contains("needs an api key") || l.contains("malformed api key") || l.contains("only available on this computer") || l.contains("not allowed") {
        "forbidden"
    // "state is open, closed or merged, not bogus", "until: a, b or c, not x" (ARCADE #13: they came back as `error`).
    } else if l.contains(" required") || l.starts_with("bad request") || l.contains("is not a ") || l.contains(" must be ") || l.starts_with("give ") || l.contains(", not ") {
        "bad_args"
    } else if l.contains("not installed") || l.contains("isn't running here") || (l.contains("borrowed") && l.contains("install")) {
        "not_installed"
    } else {
        "error"
    }
}

/// An error answer: the text and its code.
pub fn error_reply(e: &str) -> Value {
    let code = error_code(e);
    let mut v = json!({"ok": false, "error": e, "code": code});
    // A call that came too early (the ARCADE chain not reachable yet, crate::arcsvc::reach_pending_error) says when to
    // ask again, as the answer during startup does (2026-11-21).
    if code == "starting" {
        v["starting"] = json!(true);
        v["state"] = json!("starting");
        v["retry_after_s"] = json!(5);
    }
    v
}

fn handle(node: Arc<Node>, s: UnixStream) {
    let mut w = s.try_clone().unwrap();
    let mut lines = BufReader::new(s).lines();
    while let Some(Ok(line)) = lines.next() {
        // While arcaded is starting, only a stop is taken (it is acted on once the stop watcher runs); anything else
        // waits a moment, then is answered "starting".
        let stop = serde_json::from_str::<Value>(&line).is_ok_and(|r| r["cmd"] == "service.stop");
        // Once a stop began, only another stop is taken (the second one leaves without writing the caches, as a
        // second signal does not: the socket's stop only asks once).
        if !stop && crate::STOP_ASKED.load(std::sync::atomic::Ordering::SeqCst) {
            if writeln!(w, "{}", stopping_reply()).is_err() {
                break;
            }
            continue;
        }
        if !wait_ready(std::time::Duration::ZERO) {
            if !stop && !wait_ready(STARTING_WAIT) {
                if writeln!(w, "{}", starting_reply()).is_err() {
                    break;
                }
                continue;
            }
        }
        // Services over the mesh (crate::svcmesh): a helper keeps this connection; an ask with "stream" answers in
        // several lines, the last the usual {ok, result}.
        if let Ok(req) = serde_json::from_str::<Value>(&line) {
            if req["cmd"] == "svc.attach" {
                return crate::svcmesh::attach(node, &mut lines, w, &req);
            }
            if req["cmd"] == "svc.ask" && req["stream"] == true {
                let mut ww = match w.try_clone() {
                    Ok(x) => x,
                    Err(_) => return,
                };
                let r = crate::svcmesh::ask_cmd(&node, &req, Some(&mut |v: Value| writeln!(ww, "{v}").map_err(|e| e.to_string())));
                let reply = match r {
                    Ok(v) => json!({"ok": true, "result": v}),
                    Err(e) => error_reply(&e),
                };
                if writeln!(w, "{reply}").is_err() {
                    break;
                }
                continue;
            }
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            // Stop cleanly (write the coin caches, then exit), as SIGTERM does. Socket only,
            // never over HTTP: `arcade stop`, the way to stop a background arcaded on Windows.
            Ok(req) if req["cmd"] == "service.stop" => {
                crate::STOP_ASKED.store(true, std::sync::atomic::Ordering::SeqCst);
                json!({"ok": true, "result": "stopping: writing the coin caches, then exiting"})
            }
            Ok(req) => match dispatch(&node, &req) {
                Ok(v) => json!({"ok": true, "result": v}),
                Err(e) => error_reply(&e),
            },
            Err(e) => error_reply(&format!("bad request: {e}")),
        };
        if writeln!(w, "{reply}").is_err() {
            break;
        }
    }
}

/// Make this arcade's wallet: new words, or restored from `phrase` (with its @handle, when the
/// chain already shows it). New and restored wallets alike ask the mesh for a test coin.
pub fn create_wallet(node: &Node, phrase: Option<&str>) -> Result<Value, String> {
    let restored = phrase.is_some();
    let words = node.app.create_wallet(phrase).map_err(|e| e.to_string())?;
    // The wallet is saved: answer now. Finding a restored wallet's @handle can take minutes on a fresh install (its
    // tag index behind, other arcades asked over the mesh for each key slot), and a caller waiting for it gave up with
    // "timed out" though the words were right (a Kiko laptop, 2026-11-24). The handle comes in the background, shown
    // once found (Home, wallet.status); the faucet asks follow it.
    let tag = if restored { crate::claims::recorded_tag(node) } else { None };
    match node.try_arc() {
        Some(n) => {
            let _ = std::thread::Builder::new().name("wallet after create".into()).spawn(move || after_create(&n, restored));
        }
        // A bare node (tests): no thread to hand it to.
        None => after_create(node, restored),
    }
    Ok(json!({"phrase": if restored { Value::Null } else { json!(words) }, "restored": restored, "tag": tag}))
}

/// What follows a new or restored wallet, off the caller's clock: its @handle found on chain (restored), else test
/// coins for the first handle, and this arcade's ARCADE from the node faucet.
fn after_create(node: &Node, restored: bool) {
    let p = node.params;
    // A restored wallet's @handle, found on chain right away (else after a later sync round).
    let tag = if restored { crate::claims::find_own_tag(node, true) } else { None };
    // Test coins for the first handle come by themselves: asked for now, so they are
    // usually here by the time a handle is picked (a restored wallet may have some).
    // On ARCADE as the home chain (crate::claims::claim_home) the node faucet's ARCADE pays the claim instead.
    // Only while handles are claimed on Pepecoin testnet (ARCADE stalled): a fresh install still reaching ARCADE waits for
    // ARCADE's own faucet instead (step 3 of no home chain).
    if tag.is_none() && crate::claims::claim_home(node).chain_no == node.params.chain_no {
        if let Ok((k, _)) = node.app.tag_key(p, 0) {
            let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&k.pubkey).unwrap()).unwrap());
            if crate::claims::coins(node, &script).is_empty() {
                let _ = crate::faucet::ask_if_needed(node, &k.address, "");
            }
        }
    }
    // And this arcade's 1 ARCADE from the node faucet (crate::arcfaucet), asked now rather than at the next round.
    crate::arcfaucet::node_gift_ask(node);
}

pub fn dispatch(node: &Node, req: &Value) -> Result<Value, String> {
    // `"as": "@tag"` on an ARCADE write (ledger.*, a.*, arc.blob_publish*, arc.blob_upgrade): signed by the key holding that tag, the
    // wallet's own or an imported one (crate::arcledger::with_sign_as).
    let cmd = req["cmd"].as_str().unwrap_or("");
    // One table for every surface (crate::commands): a name it doesn't list is no command here either, so the socket
    // answers nothing the web API and JSON-RPC can't (the socket's own lines, commands::PRIVATE, never come here).
    if crate::commands::find(cmd).is_none() {
        return Err(format!("unknown command {cmd:?}: rpc.help lists them"));
    }
    if let Some(tag) = req["as"].as_str().filter(|_| cmd.starts_with("ledger.") || cmd.starts_with("a.") || cmd.starts_with("arc.blob_publish") || cmd == "arc.blob_upgrade" || cmd == "apps.publish" || cmd == "apps.transfer" || cmd == "apps.withdraw") {
        return crate::arcledger::with_sign_as(Some(tag.to_string()), || dispatch_inner(node, req));
    }
    // `"chain": key` on a post, act, follow or message: written there (crate::msgsvc::write_chains; option C).
    if let Some(chain) = req["chain"].as_str().filter(|_| matches!(cmd, "feed.post" | "feed.act" | "follow" | "msg.send")) {
        return crate::msgsvc::with_write_on(node, Some(chain), || dispatch_inner(node, req));
    }
    let out = dispatch_inner(node, req);
    // A command that can change a chain or the wallet: chains.list's copy is made again.
    use crate::commands::Perm;
    if crate::commands::find(cmd).is_some_and(|c| matches!(c.perm, Perm::Admin | Perm::Wallet | Perm::Local)) && cmd != "ui.link" {
        crate::chains::list_changed(node);
    }
    out
}

fn dispatch_inner(node: &Node, req: &Value) -> Result<Value, String> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    // Sends leave a receipt, and a client_ref makes a retry return the first answer (crate::receipts).
    if crate::receipts::is_send(cmd) {
        return crate::receipts::run(node, cmd, req, || dispatch_cmd(node, req));
    }
    dispatch_cmd(node, req)
}

/// `status chain=<key>` (ARCADE #25): one chain in a few fields: its tip, the best height peers tell of and how many
/// told it, how far behind, where it is read from, and the rules warning for the ARCADE chain. Never 100% made up:
/// null where no peer has told a height.
fn chain_status_short(node: &Node, key: &str) -> Result<Value, String> {
    let p = crate::chains::find(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let st = crate::chains::status(node, p.key)?;
    let blocks = st["stages"].as_array().and_then(|a| a.iter().find(|x| x["name"] == "Blocks" || x["name"] == "Headers").cloned()).unwrap_or(Value::Null);
    let tip = st["tip"].as_u64().or_else(|| blocks["at"].as_u64());
    let lenders: Vec<u64> = st["lenders"].as_array().into_iter().flatten().filter_map(|l| l["tip"].as_u64()).collect();
    let best = st["best_known"].as_u64().or_else(|| blocks["of"].as_u64().filter(|b| *b > 0)).or_else(|| lenders.iter().max().copied()).map(|b| b.max(tip.unwrap_or(0)));
    let from = if st["best_known_from"].is_u64() { st["best_known_from"].clone() } else if !lenders.is_empty() { json!(lenders.len()) } else { Value::Null };
    let tip_hash = st["tip_hash"].as_str().map(String::from).or_else(|| {
        let h = tip? as u32;
        node.chain.hash_at(p.chain_no, h).ok().flatten().map(|x| x.to_hex())
    });
    let behind = match (tip, best) {
        (Some(t), Some(b)) => Some(b.saturating_sub(t)),
        _ => None,
    };
    let installed = st["installed"] == true;
    let borrowed = st["borrowed"] == true || st["mode"] == "borrowed";
    // Nothing heard of this chain: no block of it here and no peer or lender telling a height (ARCADE #13: a borrowed
    // chain nobody lends answered "synced", behind 0, with tip 0 and no hash). Borrowed and nobody lending: waiting.
    let heard = tip_hash.is_some() || !from.is_null() || best.is_some_and(|b| b > 0);
    let sync = if !installed && !borrowed {
        "not_kept"
    } else if !heard {
        if borrowed && lenders.is_empty() { "waiting" } else { "unknown" }
    } else {
        match behind {
            None => "unknown",
            Some(0..=2) => "synced",
            Some(_) => "syncing",
        }
    };
    let arc = crate::arcsvc::defined().is_some_and(|d| d.chain_no == p.chain_no);
    let mut v = json!({
        "chain": p.key, "name": p.name, "phase": phase(), "installed": installed, "mode": st["mode"], "source": crate::explorer::source_of(node, p), "source_detail": st["source"],
        "sync": sync, "state": st["state"], "tip": tip.map(|t| json!({"height": t, "hash": tip_hash})),
        "best_known": best, "best_known_from": from, "best_known_age_s": st["best_known_age_s"], "behind": behind,
        "peer_tips": st["peer_tips"], "overall": st["overall"], "waiting": st["waiting"],
        "unit": crate::explorer::unit(p), "decimals": 8,
    });
    if arc {
        v["rules"] = crate::arcsvc::status(node).ok().map(|a| a["rules"].clone()).filter(|r| !r.is_null()).unwrap_or_else(|| json!({"update_needed": crate::arcsvc::update_needed()}));
    }
    Ok(v)
}

/// `ledger.items` with filters (ARCADE #25): state (open, closed, merged), kind (issue, pull), since (items or
/// events at or after this height), summary (true: no events, the text cut to its title line).
fn filter_items(mut v: Value, req: &Value) -> Result<Value, String> {
    let state = req["state"].as_str();
    let kind = req["kind"].as_str();
    if let Some(k) = kind.filter(|k| !matches!(*k, "issue" | "pull")) {
        return Err(format!("kind is issue or pull, not {k}"));
    }
    if let Some(x) = state.filter(|x| !matches!(*x, "open" | "closed" | "merged")) {
        return Err(format!("state is open, closed or merged, not {x}"));
    }
    let since = req["since"].as_u64().or_else(|| req["since"].as_str().and_then(|x| x.parse().ok()));
    let summary = req["summary"] == true;
    if let Some(items) = v["items"].as_array_mut() {
        items.retain(|it| {
            state.is_none_or(|x| it["state"] == x)
                && kind.is_none_or(|k| it["kind"] == k)
                && since.is_none_or(|h| {
                    it["pending"] == true
                        || it["height"].as_u64().is_some_and(|ih| ih >= h) || it["events"].as_array().into_iter().flatten().any(|e| e["height"].as_u64().is_some_and(|eh| eh >= h))
                })
        });
        for it in items.iter_mut() {
            it["comments"] = json!(it["events"].as_array().map(|a| a.iter().filter(|e| e["w"] == "comment" || e["w"] == "review").count()).unwrap_or(0));
            // 2026-11-21 (ARCADE #13): every item says when it last moved, so an agent can tell whether to read it again.
            // last_height is the newest confirmed event's height (the item's own height when it has none yet), taken
            // before since trims the events; pending events have no height and do not count.
            let (last_height, last_by) = last_activity(it);
            it["last_height"] = last_height;
            if summary {
                if let Some(o) = it.as_object_mut() {
                    o.remove("events");
                }
                let title = it["text"]["title"].clone();
                it["text"] = json!({"title": title});
                // 2026-11-21 (ARCADE #13): a summary also names who made that newest event.
                it["last_by"] = last_by;
            } else if let Some(h) = since {
                // 2026-11-21 (ARCADE #13): with since, keep only the events at or after it (pending ones are newer than
                // any height, so they stay). The item itself is still chosen as before, and comments still counts all.
                if let Some(ev) = it["events"].as_array_mut() {
                    ev.retain(|e| e["pending"] == true || e["height"].as_u64().is_none_or(|eh| eh >= h));
                }
            }
        }
        let n = items.len();
        v["count"] = json!(n);
    }
    Ok(v)
}

/// An item's newest confirmed event: (its height, its author), or the item's own height and author when no event has
/// a height yet (2026-11-21, ARCADE #13). Ties go to the later event in the list, the order the ledger keeps them in.
fn last_activity(it: &Value) -> (Value, Value) {
    let mut best: (Value, Value) = (it["height"].clone(), it["by"].clone());
    let mut top = it["height"].as_u64();
    for e in it["events"].as_array().into_iter().flatten() {
        if let Some(h) = e["height"].as_u64() {
            if top.is_none_or(|t| h >= t) {
                top = Some(h);
                best = (json!(h), e["by"].clone());
            }
        }
    }
    best
}

/// Most ledger.items on one page.
const ITEMS_MAX: usize = 500;

/// `ledger.items` a page at a time (ARCADE #25 part 2): numbered items in order after `cursor` (the `next` of the
/// page before, opaque to callers), at most `limit`; the last page (`next` null) also carries the pending ones.
fn page_items(mut v: Value, req: &Value) -> Result<Value, String> {
    let limit = req["limit"].as_u64().or_else(|| req["limit"].as_str().and_then(|x| x.parse().ok())).unwrap_or(50).clamp(1, ITEMS_MAX as u64) as usize;
    let after: Option<u64> = match req["cursor"].as_str().filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => Some(c.strip_prefix("n.").and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad request: cursor {c} is not one ledger.items gave"))?),
    };
    let Some(items) = v["items"].as_array_mut() else { return Ok(v) };
    let all = std::mem::take(items);
    let (mut numbered, pending): (Vec<Value>, Vec<Value>) = all.into_iter().partition(|it| it["number"].is_u64());
    numbered.sort_by_key(|it| it["number"].as_u64().unwrap_or(0));
    numbered.retain(|it| after.is_none_or(|a| it["number"].as_u64().unwrap_or(0) > a));
    let more = numbered.len() > limit;
    numbered.truncate(limit);
    let next = if more { numbered.last().and_then(|it| it["number"].as_u64()).map(|n| format!("n.{n}")) } else { None };
    if next.is_none() {
        numbered.extend(pending);
    }
    let n = numbered.len();
    v["items"] = json!(numbered);
    v["count"] = json!(n);
    v["next"] = json!(next);
    v["limit"] = json!(limit);
    Ok(v)
}

/// `events.since {cursor?, kinds?, limit?, wait?}`: the event log after a cursor, oldest first (ARCADE #25 part 2).
/// No cursor: from now (only what comes next); "0": from the oldest event kept.
fn events_since(node: &Node, req: &Value) -> Result<Value, String> {
    let limit = req["limit"].as_u64().or_else(|| req["limit"].as_str().and_then(|x| x.parse().ok())).unwrap_or(100).clamp(1, 1000) as usize;
    let wait = req["wait"].as_u64().or_else(|| req["wait"].as_str().and_then(|x| x.parse().ok())).unwrap_or(0).min(60);
    let kinds: Option<Vec<String>> = match &req["kinds"] {
        Value::Null => None,
        Value::String(s) => Some(s.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect()),
        Value::Array(a) => Some(a.iter().filter_map(|k| k.as_str().map(String::from)).collect()),
        _ => return Err("bad request: kinds is a comma list or an array of kinds".into()),
    };
    let cur = match &req["cursor"] {
        Value::Number(n) if n.as_u64() == Some(0) => Some("0".to_string()),
        v => v.as_str().map(String::from),
    };
    let after: Option<Vec<u8>> = match cur.as_deref() {
        None | Some("") => Some(node.app.last_event_key().map_err(|e| e.to_string())?.unwrap_or_default()),
        Some("0") => None,
        Some(c) => Some(c.strip_prefix("e.").and_then(|h| hex::decode(h).ok()).ok_or_else(|| format!("bad request: cursor {c} is not one events.since gave"))?),
    };
    let end = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    let mut cursor = after.clone().filter(|a| !a.is_empty());
    loop {
        let rows = node.app.events_after(cursor.as_deref(), 2000).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut last = cursor.clone();
        for (k, e) in rows {
            last = Some(k.clone());
            if kinds.as_ref().is_none_or(|ks| ks.iter().any(|x| *x == e.kind)) {
                out.push(json!({"cursor": format!("e.{}", hex::encode(&k)), "at": e.at, "kind": e.kind, "text": e.text}));
                if out.len() >= limit {
                    break;
                }
            }
        }
        cursor = last;
        let done = !out.is_empty() || std::time::Instant::now() >= end || crate::STOP_ASKED.load(std::sync::atomic::Ordering::Relaxed);
        if done {
            let next = cursor.as_ref().map(|k| format!("e.{}", hex::encode(k))).unwrap_or_else(|| "0".into());
            return Ok(json!({"events": out, "next": next, "waited": wait > 0, "timed_out": out.is_empty() && wait > 0}));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn dispatch_cmd(node: &Node, req: &Value) -> Result<Value, String> {
    let s = |k: &str| req[k].as_str().map(String::from);
    let p = node.params;
    let c = p.chain_no;
    match req["cmd"].as_str().unwrap_or("") {
        // The link that opens the desktop app's window in a browser (crate::webui; `arcade web`).
        "ui.link" => crate::webui::link(node, node.http_port.get().copied().unwrap_or(8420)),
        // Every chain named gets the same short answer, the first one too (ARCADE #13, 2026-11-21: `status {chain:
        // peptest}` came back in the old shape, with no behind, mode, source or peer_tips).
        "status" if req["chain"].as_str().is_some() => chain_status_short(node, req["chain"].as_str().unwrap()),
        "status" => {
            let tip = node.chain.tip(c).map_err(|e| e.to_string())?;
            use arcade_ledger::index;
            // The tags read once for both the count and the digest.
            let tags = index::read_tags(&node.chain, c).ok();
            let tag_count = tags.as_ref().map(|t| t.len()).unwrap_or(0);
            let digest = tags.as_ref().map(|t| arcade_ledger::tags::digest(t.iter().map(|(n, r)| (n.as_str(), r)))).unwrap_or_default();
            let indexed_to = node.chain.indexed_to(c, "ledger.v1").ok().flatten();
            // The best height anyone tells us of (headers, arcades on the mesh, lenders); read before mesh_tips is held.
            let best = tip.as_ref().map(|t| t.height).unwrap_or(0) + crate::borrow::home_lag(node).0;
            let tips = node.mesh_tips.lock().unwrap();
            let mesh: Vec<Value> = tips
                .iter()
                .map(|(a, t)| json!({"peer": a, "height": t.height, "names_digest": t.names_digest}))
                .collect();
            // The first chain's sync as `status {chain}` says it (ARCADE #13: "starting" while phase was "ready").
            let short = chain_status_short(node, p.key).ok();
            let sync_state = node.sync_state.lock().unwrap().clone();
            let sync = short.as_ref().and_then(|v| v["sync"].as_str().map(String::from)).filter(|_| phase() == "ready").unwrap_or(sync_state);
            Ok(json!({
                "chain": p.name,
                "sync": sync,
                "behind": short.as_ref().map(|v| v["behind"].clone()),
                "tip": tip.map(|t| json!({"height": t.height, "hash": t.hash.to_hex()})),
                "best_known": best,
                "tags": {"indexed_to": indexed_to, "count": tag_count, "names_digest": digest},
                "profiles": index::profile_count(&node.chain, c).unwrap_or(0),
                "mesh": {"links": node.links.lock().unwrap().len(), "tips": mesh},
                "node_id": node.id_hex(),
                "wallet": node.app.has_wallet().unwrap_or(false),
                "key": p.key,
                "phase": phase(),
                // The ARCADE rules warning where agents look (ARCADE #25: it was only in arc.status).
                "rules": {"update_needed": crate::arcsvc::update_needed()},
                "chains": "status chain=<key> for one chain (the ARCADE chain: arctest)",
            }))
        }
        "mesh.udp" => Ok(crate::udpsvc::status(node)),
        "mesh.sysinfo" => Ok(crate::sysinfo::read(node)),
        "cpu.status" => Ok(crate::cpu::status()),
        "cpu.set" => crate::cpu::set(&s("level").ok_or("level required: low, balanced or full")?),
        "screening.status" => Ok(crate::screening::status(node)),
        "screening.set" => crate::screening::set(node, &req),
        "screening.test" => crate::screening::test(node),
        "screening.topup" => crate::screening::topup(node, &req),
        "sysinfo.status" => Ok(crate::sysinfo::status(node)),
        "sysinfo.set" => crate::sysinfo::set(node, &req),
        "mesh.relay" => Ok(crate::relaysvc::status(node)),
        "svc.offers" => crate::svcmesh::list_offers(node, req),
        "svc.services" => Ok(crate::svcmesh::services()),
        "svc.ask" => crate::svcmesh::ask_cmd(node, req, None),
        "svc.pay" => crate::svcmesh::pay(node, req),
        "svc.paid" => crate::svcmesh::paid(node, req),
        "svc.attach" => Err("svc.attach is for the API socket only (it keeps the connection)".into()),
        "mesh.dials" => Ok(crate::meshsvc::dials_status()),
        "mesh.name" => Ok(json!({"name": crate::meshsvc::public_name(node)})),
        "mesh.name_set" => crate::meshsvc::set_public_name(node, req["name"].as_str().ok_or("name required (\"\" clears it)")?),
        "mesh.relay_set" => crate::relaysvc::set(node, &req),
        "mesh.udp_set" => {
            if let Some(p) = req["drop"].as_str().filter(|p| !p.is_empty()) {
                return Ok(json!({"dropped": crate::udpsvc::drop_path(p)}));
            }
            crate::udpsvc::set(node, req)
        }
        "peers" => Ok(json!(node.links.lock().unwrap().values().map(|l| json!({"addr": l.addr, "node_id": l.node_id, "since": l.since})).collect::<Vec<_>>())),
        "wallet.create" => create_wallet(node, s("phrase").as_deref()),
        "wallet.phrase" => Ok(json!({"phrase": node.app.phrase().map_err(|e| e.to_string())?})),
        "wallet.keys" => {
            // Any chain: the same 12 words derive each chain's own addresses (its BIP44 coin type).
            let cp = match s("chain") {
                Some(k) => crate::chains::find(&k).ok_or_else(|| format!("unknown chain {k}"))?,
                None => p,
            };
            let mut keys = node.app.tag_keys(cp.chain_no).map_err(|e| e.to_string())?;
            if keys.is_empty() && node.app.has_wallet().map_err(|e| e.to_string())? {
                keys.push(node.app.tag_key(cp, 0).map_err(|e| e.to_string())?.0);
            }
            // A borrowed copy answers while the installed one catches up.
            let installed = !crate::borrow::is_borrowed(cp.key) && crate::chains::kept(node, cp) && node.chain.addresses_on(cp.chain_no).unwrap_or(false);
            let multi = arcade_wallet::keys::kinds_for(cp).len() > 1;
            let pref = crate::coinsend::pref_kind(node, cp);
            Ok(json!(keys.iter().map(|k| {
                let pk = arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&k.pubkey).unwrap()).unwrap();
                // Every address of the key (DigiByte: dgb1... to receive, its D... still counted).
                let scripts = crate::coinsend::key_scripts(cp, &pk);
                // As tx.coins counts them, on any chain with a pool here: confirmed coins less those a waiting
                // transaction spends, plus waiting outputs to this wallet (ARCADE #15). On SegWit chains every
                // address type's scripts count (each type's own figure under kinds).
                let own = if multi { crate::coinsend::own_addresses(node, cp, k) } else { Vec::new() };
                let mut sc: Vec<Vec<u8>> = scripts.iter().map(|(_, s)| s.clone()).collect();
                for (_, v) in &own {
                    for (_, s) in v {
                        if !sc.contains(s) {
                            sc.push(s.clone());
                        }
                    }
                }
                let counted = installed || crate::borrow::is_borrowed(cp.key);
                let f = if counted { crate::sitesvc::wallet_figures(node, cp, &sc) } else { None };
                let balance = f.as_ref().map(|f| f.balance);
                // Address types (SegWit chains): each kind's address and coins; `balance` counts them all.
                let kinds: Vec<Value> = own
                    .into_iter()
                    .map(|(kd, v)| {
                        let ks: Vec<Vec<u8>> = v.iter().map(|(_, s)| s.clone()).collect();
                        let kb = if counted { crate::sitesvc::wallet_figures(node, cp, &ks).map(|f| f.balance) } else { None };
                        json!({"id": kd.id(), "label": kd.label(), "path": kd.path(cp.bip44_coin_type, k.index),
                               "address": v[0].0, "legacy": v.get(1).map(|x| x.0.clone()), "balance": kb})
                    })
                    .collect();
                let waiting: Vec<_> = f.as_ref().map(|f| &f.waiting[..]).unwrap_or_default().iter()
                    .map(|(id, received, spent)| json!({"txid": id.to_hex(), "received": received, "spent": spent})).collect();
                // Own transactions that left the pool without a block (crate::ownwait): their coins are free again.
                let dropped: Vec<_> = crate::ownwait::dropped(node, cp, |s| sc.iter().any(|x| x.as_slice() == s)).into_iter()
                    .map(|(id, received, spent)| json!({"txid": id.to_hex(), "received": received, "spent": spent})).collect();
                json!({"index": k.index, "address": scripts[0].0, "legacy": scripts.get(1).map(|x| x.0.clone()),
                       // confirmed: the blocks' figure; free: confirmed coins spendable now (no waiting transaction spends
                       // them, mature, no inscription); pending: waiting outputs arriving (pending_coins of them).
                       "dropped": dropped,
                       "confirmed": f.as_ref().map(|f| f.confirmed), "free": f.as_ref().map(|f| f.free),
                       "pending": f.as_ref().map_or(0, |f| f.arriving), "pending_coins": f.as_ref().map_or(0, |f| f.arriving_coins),
                       "waiting": waiting,
                       // @tags live on the home chain and on ARCADE (running here): there, the tag this address holds on
                       // ARCADE's own ledger, as tag.check reads it (a tag carried over at block 0 was never claimed
                       // from this wallet, so the key has none recorded).
                       "tag": if cp.chain_no == p.chain_no { k.tag.clone() } else if crate::arcsvc::runs(cp) {
                           k.tag.clone().or_else(|| arcade_ledger::index::tag_of(&node.chain, cp.chain_no, &scripts[0].0).ok().flatten())
                       } else if crate::arcsvc::usable(cp) {
                           // ARCADE borrowed: the tag its claim confirmed (recorded on the key), else what keepers say.
                           k.tag.clone().or_else(|| crate::hometag::of_address(node, &scripts[0].0))
                       } else { None },
                       "balance": balance, "borrowed": !installed && crate::borrow::is_borrowed(cp.key),
                       "kinds": kinds, "kind": if multi { Some(pref.id()) } else { None }})
            }).collect::<Vec<_>>()))
        }
        "dd.wallet" => crate::ddsvc::wallet(node, s("chain").as_deref()),
        "dd.coins" => crate::ddsvc::coins_of(node, s("chain").as_deref(), &s("pubkey").ok_or("pubkey required (a DigiByte key, 33 bytes of hex)")?),
        "dd.send" => {
            let cents = req["cents"].as_i64().ok_or("cents required (DigiDollar cents: $12.50 is 1250)")?;
            crate::ddsvc::send(node, s("chain").as_deref(), &s("to").ok_or("to required (a DD... or TD... address)")?, cents, req["send"] == true)
        }
        "wallet.send" => {
            let amount = req["amount"].as_u64().ok_or("amount required (satoshis)")?;
            crate::coinsend::OWNER_CHANGE_OK.with(|c| c.set(req["owner_changed_ok"] == true));
            let picked = crate::coinctl::parse_coins(&req["coins"])?;
            let r = crate::coinsend::send(node, s("chain").as_deref(), &s("to").ok_or("to required")?, amount, s("from").as_deref(), s("kind").as_deref(), req["all_kinds"] == true, req["send"] == true, &picked);
            crate::coinsend::OWNER_CHANGE_OK.with(|c| c.set(false));
            r
        }
        "wallet.coins" => crate::coinctl::coins(node, s("chain").as_deref(), s("from").as_deref(), s("kind").as_deref(), req["all_kinds"] == true),
        "wallet.freeze" => crate::coinctl::freeze(node, &s("chain").ok_or("chain required")?, &crate::coinctl::parse_coins(&req["coins"])?, req["frozen"] != false),
        "wallet.coin_prefs" => crate::coinctl::set_prefs(node, req["coin_control"].as_bool(), req["tip"].as_bool()),
        "wallet.addr_kind" => {
            let cp = crate::chains::find(&s("chain").ok_or("chain required")?).ok_or("unknown chain")?;
            crate::coinsend::addr_kind(node, cp, s("kind").as_deref())
        }
        "wallet.link" => crate::links::link(node, &s("chain").ok_or("chain required")?, &s("wif").ok_or("wif required")?, &s("tag").ok_or("tag required")?),
        "wallet.unlink" => crate::links::unlink(node, &s("chain").ok_or("chain required")?, &s("address").ok_or("address required")?, &s("tag").ok_or("tag required")?),
        "account.signin" => crate::accountsvc::sign_in(node, &s("tag").ok_or("tag required")?, &s("password").ok_or("password required")?, s("from").as_deref().filter(|f| !f.is_empty())),
        "account.add" => crate::accountsvc::add_words(node, &s("phrase").ok_or("phrase required")?),
        "account.list" => Ok(crate::accountsvc::list(node)),
        "account.switch" => crate::accountsvc::switch(node, &s("id").unwrap_or_default()),
        "account.signout" => crate::accountsvc::sign_out(node, &s("id").ok_or("id required")?),
        "wallet.import" => crate::importsvc::import(node, &s("chain").ok_or("chain required")?, &s("secret").ok_or("secret required (a WIF key or a wallet's words)")?, &s("addr_kind").unwrap_or_default(), &s("name").unwrap_or_default(), "manual"),
        "wallet.imports" => Ok(crate::importsvc::list(node)),
        // Multisig vaults, watch-only (crate::msigsvc; docs/MULTISIG.md phase 4).
        "multisig.key" => crate::msigsvc::key(node, &s("chain").ok_or("chain required")?, &s("wrap").unwrap_or_default()),
        "multisig.create" => {
            let keys: Vec<String> = match &req["cosigners"] {
                Value::Array(a) => a.iter().filter_map(|k| k.as_str().map(String::from)).collect(),
                Value::String(t) => t.split(['\n', ' ']).filter(|k| !k.trim().is_empty()).map(String::from).collect(),
                _ => Vec::new(),
            };
            crate::msigsvc::create(node, &s("chain").ok_or("chain required")?, &s("name").unwrap_or_default(), &keys, &s("wrap").unwrap_or_default(), &s("backup").unwrap_or_default())
        }
        "multisig.import" => crate::msigsvc::import(node, &s("chain").ok_or("chain required")?, &s("descriptor").ok_or("descriptor required")?, &s("name").unwrap_or_default(), &s("backup").unwrap_or_default()),
        "multisig.list" => Ok(crate::msigsvc::list(node, s("chain").as_deref())),
        "multisig.address" => {
            let index = match &req["index"] {
                Value::Number(n) => Some(n.as_u64().filter(|&i| i < 1 << 31).ok_or("index is a whole number below 2^31")? as u32),
                Value::String(t) if !t.is_empty() => Some(t.parse::<u32>().ok().filter(|&i| i < 1 << 31).ok_or("index is a whole number below 2^31")?),
                _ => None,
            };
            crate::msigsvc::address(node, &s("id").ok_or("id required")?, index, req["change"].as_bool().unwrap_or(false))
        }
        "multisig.coins" => crate::msigsvc::coins_with(node, &s("id").ok_or("id required")?, req["lenders"].as_bool().unwrap_or(true)),
        // Spending from a vault (crate::msigspend; docs/MULTISIG.md phase 5).
        "multisig.propose" => {
            let all = req["all"].as_bool().unwrap_or(false);
            let amount = match &req["amount"] {
                Value::Number(n) => n.as_u64().ok_or("amount is a whole number of the smallest unit")?,
                Value::String(t) if !t.is_empty() => t.parse().map_err(|_| "amount is a whole number of the smallest unit")?,
                _ if all => 0,
                _ => return Err("amount required (or all: true)".into()),
            };
            let coins: Vec<String> = match &req["coins"] {
                Value::Array(a) => a.iter().filter_map(|c| c.as_str().map(String::from)).collect(),
                Value::String(t) => t.split([',', ' ', '\n']).filter(|c| !c.is_empty()).map(String::from).collect(),
                _ => Vec::new(),
            };
            crate::msigspend::propose(node, &s("id").ok_or("id required")?, &s("to").ok_or("to required")?, amount, all, &coins)
        }
        "multisig.card" => crate::msigspend::card(node, &s("id").ok_or("id required")?, s("psbt").as_deref(), s("proposal").as_deref()),
        "multisig.merge" => crate::msigspend::merge(node, &s("id").ok_or("id required")?, &s("psbt").ok_or("psbt required")?),
        "multisig.sign" => crate::msigspend::sign(node, &s("id").ok_or("id required")?, s("psbt").as_deref(), s("proposal").as_deref()),
        "multisig.finalize" => crate::msigspend::finalize(node, &s("id").ok_or("id required")?, s("psbt").as_deref(), s("proposal").as_deref(), req["send"].as_bool().unwrap_or(false)),
        "multisig.drop" => crate::msigspend::drop_proposal(node, &s("id").ok_or("id required")?, &s("proposal").ok_or("proposal required")?),
        // Co-signing over the mesh (crate::msigmesh; docs/MULTISIG.md phase 6).
        "multisig.set_node" => crate::msigmesh::set_node(node, &s("id").ok_or("id required")?, req["place"].as_u64().ok_or("place required (the co-signer's place in the vault, from 0)")? as usize, &s("arcade").unwrap_or_default()),
        "multisig.share" => crate::msigmesh::share(node, &s("id").ok_or("id required")?, &s("proposal").ok_or("proposal required")?),
        "multisig.remove" => crate::msigsvc::remove(node, &s("id").ok_or("id required")?),
        "wallet.import_remove" => crate::importsvc::remove(node, &s("chain").ok_or("chain required")?, &s("address").ok_or("address required")?),
        "wallet.links" => {
            let chain = s("chain").ok_or("chain required")?;
            let mut v = crate::links::mine(node, &chain)?;
            // Borrowed chains: balances from the borrowed coins.
            if let Some(cp) = crate::chains::find(&chain).filter(|cp| crate::borrow::is_borrowed(cp.key)) {
                for l in v.iter_mut() {
                    if let Some(sc) = l["address"].as_str().and_then(|a| arcade_ledger::address::script_for_address(cp, a)) {
                        l["balance"] = json!(crate::borrow::balance(node, cp, &sc));
                    }
                }
            }
            Ok(json!(v))
        }
        // Messaging (home chain): the Messages tab and anything else that wants it.
        "msg.me" => crate::msgsvc::me(node),
        "ledger.write_chains" => Ok(crate::msgsvc::write_chains_status(node)),
        // Posts, acts, message candidates and profiles read from lenders for chains this arcade can't read itself yet.
        "ledger.lent" => Ok(crate::lentrows::status(node)),
        "msg.stats" => {
            let home = node.ledger_home();
            let mut v = arcade_ledger::messages::counts(&node.chain, home.chain_no);
            // The ledger home borrowed or catching up: the rows read from lenders too, marked as lent (crate::lentrows).
            if crate::lentrows::lent(node, home) {
                let lent: serde_json::Map<String, Value> = ["post", "act", "cand", "profile"].iter()
                    .map(|t| (t.to_string(), json!(crate::lentrows::rows(node, home, *t).len()))).collect();
                if let Some(o) = v.as_object_mut() {
                    o.insert("lent".into(), Value::Object(lent));
                }
            }
            Ok(v)
        }
        "msg.scan" => Ok(json!({"opened": crate::msgsvc::scan(node)?})),
        "msg.who" => crate::msgsvc::who(node, &s("name").ok_or("name required (@tag or address)")?),
        "msg.conversations" => crate::msgsvc::conversations(node, req["requests"] == true),
        "msg.thread" => crate::msgsvc::thread(node, &s("peer").ok_or("peer required (messaging key)")?),
        "msg.read" => crate::msgsvc::mark_read(node, &s("peer").ok_or("peer required")?),
        "msg.unread" => Ok(crate::msgsvc::unread(node)),
        "msg.file" => crate::msgsvc::file(node, &s("id").ok_or("id required")?),
        "msg.send" => {
            let file = match req["file"].as_object() {
                Some(f) => {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(f.get("data").and_then(|d| d.as_str()).unwrap_or("")).map_err(|e| format!("file data: {e}"))?;
                    Some(arcade_msg::content::File { name: f.get("name").and_then(|x| x.as_str()).unwrap_or("file").into(), mime: f.get("type").and_then(|x| x.as_str()).unwrap_or("application/octet-stream").into(), bytes })
                }
                None => None,
            };
            crate::msgsvc::send(node, &s("to").ok_or("to required (@tag, address or messaging key)")?, &s("text").unwrap_or_default(), file)
        }
        "feed" => crate::feedsvc::feed(node, &crate::feedsvc::View {
            order: s("order").unwrap_or_else(|| "popular".into()),
            txid: s("txid"),
            channel: s("channel").filter(|c| !c.is_empty()),
            author: match s("tag") {
                // Whoever holds it (ARCADE, the ledger home; read from lenders when those are borrowed). The feed matches
                // authors by key, so the address's chain form doesn't matter.
                Some(t) => {
                    let t = t.trim_start_matches('@').to_lowercase();
                    let (_, a) = crate::hometag::owner_or_err(node, &t)?;
                    Some(a)
                }
                None => s("author"),
            },
            limit: req["limit"].as_u64().unwrap_or(30).min(200) as usize,
            offset: req["offset"].as_u64().unwrap_or(0) as usize,
        }),
        "feed.channels" => Ok(crate::feedsvc::channels(node)),
        "feed.settings" => if req["set"].is_object() { crate::feedsvc::set_settings(node, &req["set"]) } else { Ok(crate::feedsvc::settings(node)) },
        "feed.post" => {
            let file = match req["file"].as_object() {
                Some(f) => {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(f.get("data").and_then(|d| d.as_str()).unwrap_or("")).map_err(|e| format!("file data: {e}"))?;
                    Some(arcade_msg::content::File { name: f.get("name").and_then(|x| x.as_str()).unwrap_or("file").into(), mime: f.get("type").and_then(|x| x.as_str()).unwrap_or("application/octet-stream").into(), bytes })
                }
                None => None,
            };
            crate::feedsvc::post(node, &s("text").unwrap_or_default(), &s("channel").unwrap_or_default(), file)
        }
        "feed.act" => crate::feedsvc::act(node, &s("kind").ok_or("kind required")?, &s("target").ok_or("target required (txid)")?, &s("text").unwrap_or_default(), req["amount"].as_u64().unwrap_or(0)),
        "feed.file" => crate::feedsvc::file(node, &s("txid").ok_or("txid required")?),
        "follows" => crate::follows::follows(node, &s("who").ok_or("give an @tag or an address")?),
        "follow" => crate::follows::follow(node, &s("who").ok_or("give an @tag or an address")?, req["on"] != false),
        "group.list" => Ok(crate::grpsvc::list(node)),
        "group.create" => {
            let names: Vec<String> = req["members"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::grpsvc::create(node, &s("name").ok_or("name required")?, &names, &s("description").unwrap_or_default())
        }
        "group.thread" => crate::grpsvc::thread(node, &s("id").ok_or("id required")?),
        "group.read" => crate::grpsvc::mark_read(node, &s("id").ok_or("id required")?),
        "group.say" => {
            let file = match req["file"].as_object() {
                Some(f) => {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(f.get("data").and_then(|d| d.as_str()).unwrap_or("")).map_err(|e| format!("file data: {e}"))?;
                    Some(arcade_msg::content::File { name: f.get("name").and_then(|x| x.as_str()).unwrap_or("file").into(), mime: f.get("type").and_then(|x| x.as_str()).unwrap_or("application/octet-stream").into(), bytes })
                }
                None => None,
            };
            crate::grpsvc::say(node, &s("id").ok_or("id required")?, &s("text").unwrap_or_default(), file)
        }
        "group.app_list" => crate::grpsvc::app_list(node),
        "group.app_read" => crate::grpsvc::app_read(node, &s("id").ok_or("id required")?, req["after"].as_u64().unwrap_or(0), req["limit"].as_u64().unwrap_or(200) as usize),
        "group.seal" => crate::grpsvc::seal_live(node, &s("id").ok_or("id required")?, &s("text").unwrap_or_default()),
        "group.open" => crate::grpsvc::open_live(node, &s("id").ok_or("id required")?, &s("sealed").unwrap_or_default()),
        "group.add" => {
            let names: Vec<String> = req["members"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::grpsvc::add(node, &s("id").ok_or("id required")?, &names)
        }
        "group.leave" => crate::grpsvc::leave(node, &s("id").ok_or("id required")?),
        "group.update" => crate::grpsvc::update(node, &s("id").ok_or("id required")?, &req["changes"]),
        "group.remove" => {
            let keys: Vec<String> = req["members"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::grpsvc::remove(node, &s("id").ok_or("id required")?, &keys)
        }
        "apps.store" => Ok(crate::appsvc::store_with(node, &s("sort").unwrap_or_else(|| "popular".into()), &s("q").unwrap_or_default(), req["flagged"] == true, &s("category").unwrap_or_default(), req["withdrawn"] == true)),
        "apps.publish" => crate::apppub::publish_req(node, req),
        "apps.withdraw" => crate::apppub::withdraw_req(node, req),
        "apps.transfer" => crate::apppub::transfer_req(node, req),
        "apps.categories" => Ok(crate::appsvc::categories()),
        "apps.installed" => Ok(crate::appsvc::installed(node)),
        "apps.install" => crate::appsvc::install(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?),
        "apps.uninstall" => crate::appsvc::uninstall(node, &s("id").ok_or("id required")?),
        "apps.star" => crate::appsvc::star(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?, req["on"] != false),
        // arcade.apps for app pages (2026-11-22): the rows GET /r/apps answers, and what a window needs to open another app.
        "apps.rows" => Ok(crate::appsvc::page_list(node, &s("q").unwrap_or_default(), req["offset"].as_u64().unwrap_or(0) as usize,
                                                   req["limit"].as_u64().map(|n| n as usize).unwrap_or(crate::appsvc::PAGE_ROWS_DEFAULT))),
        "apps.open" => crate::appsvc::open(node, &s("id").ok_or("id required")?, &s("from").unwrap_or_default(), &s("page").unwrap_or_default()),
        "apps.card" => crate::appsvc::card(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?),
        "apps.perms" => {
            let id = s("id").ok_or("id required")?;
            if req["set"].is_object() { crate::appsvc::set_perms(node, &id, &req["set"]) } else { Ok(crate::appsvc::perms(node, &id)) }
        }
        // Spending allowances (appsvc): by the app's id, or a page of it (the app window asks by its page).
        c @ ("apps.allowance" | "apps.allowance_use") => {
            let id = match s("id") {
                Some(id) => id,
                None => crate::appsvc::of_page(node, &s("page").ok_or("id or page required")?).and_then(|a| a["id"].as_str().map(String::from)).ok_or("no app has that page")?,
            };
            let service = s("service").ok_or("service required")?;
            if c == "apps.allowance" {
                crate::appsvc::allowance(node, &id, &service)
            } else {
                let amount = req["amount"].as_f64().ok_or("amount required (ARCADE)")?;
                crate::appsvc::allowance_use(node, &id, &service, amount, &s("note").unwrap_or_default(), req["undo"] == true)
            }
        }
        // An app's folder (crate::foldersvc, 2026-11-21): given after the person picks it on the app window's card, its
        // changes logged; the picker's subfolders; and the folder ops for programs on this computer (CLI apps), the same
        // code as the page API's /r/folder/*.
        "apps.folder_set" => crate::foldersvc::grant(node, &s("id").ok_or("id required")?, &s("path").ok_or("path required")?, &crate::foldersvc::browse_roots()),
        "apps.folder_log" => Ok(crate::foldersvc::folder_log(node, &s("id").ok_or("id required")?, req["limit"].as_u64().map(|n| n as usize))),
        "folder.browse" => crate::foldersvc::browse(req["path"].as_str()),
        c @ ("folder.list" | "folder.stat" | "folder.read" | "folder.write" | "folder.mkdir" | "folder.remove") => {
            crate::foldersvc::op(node, &s("id").ok_or("id required")?, &c["folder.".len()..], req)
        }
        "apps.review" => crate::appsvc::review(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?, &s("verdict").ok_or("verdict required (clear or confirm)")?),
        "apps.trust" => crate::appsvc::trust(node, &s("address").ok_or("address required")?, req["on"] != false),
        "inscribe" | "inscribe.estimate" => {
            use base64::Engine;
            let data = req["data"].as_str().unwrap_or("");
            let bytes = if req["encoding"] == "base64" { base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("data: {e}"))? } else { data.as_bytes().to_vec() };
            let ctype = s("contenttype").unwrap_or_else(|| "text/plain".into());
            let json = match &req["json"] { Value::Null => None, Value::String(j) => Some(j.clone()), other => Some(other.to_string()) };
            let chain = s("chain").filter(|c| !c.is_empty());
            if req["cmd"] == "inscribe.estimate" {
                crate::walletsvc::inscribe_estimate(node, chain.as_deref(), bytes.len(), &ctype, json.as_ref().map(|j| j.len()).unwrap_or(0))
            } else {
                crate::inscqueue::inscribe(node, &bytes, &ctype, json.as_deref(), chain.as_deref())
            }
        }
        "inscribe.status" => crate::inscqueue::status(node, s("id").as_deref()),
        "inscribe.resume" => crate::inscqueue::resume_req(node, req),
        "wallet.requests" => Ok(crate::walletsvc::requests(node, s("page").as_deref())),
        "wallet.preview" => crate::walletsvc::preview(node, &s("id").ok_or("id required")?),
        "wallet.approve" => crate::walletsvc::approve(node, &s("id").ok_or("id required")?),
        "wallet.deny" => crate::walletsvc::deny(node, &s("id").ok_or("id required")?),
        "apps.owner" => Ok(crate::walletsvc::owner_identity(node, &s("page").ok_or("page required")?)),
        "apps.owner_send" => crate::walletsvc::owner_send(node, &s("page").ok_or("page required")?, &req["request"]),
        "realtime.join" => {
            let who = s("member").filter(|m| !m.is_empty()).map(|m| (m, Some(req["cred"].clone()).filter(|c| !c.is_null())));
            let j = crate::realtime::join_as(node, &s("page").ok_or("page required")?, &s("game").unwrap_or_default(), &s("room").ok_or("room required")?, who)?;
            if req["fast"] == true {
                crate::realtime::fast::want_fast(j["token"].as_str().unwrap_or(""));
            }
            Ok(j)
        }
        "realtime.ice" => Ok(crate::turnsvc::ice(node, &s("tag").unwrap_or_default(), req["lan"] == true)),
        "realtime.cert" => crate::realtime::cert(node, &s("address").ok_or("address required")?),
        "id.start" => crate::siteid::start(&s("host").ok_or("host required")?, s("path").as_deref(), req["http"] == true),
        "id.callback" => crate::siteid::callback(&s("address").ok_or("address required")?, &s("uri").ok_or("uri required")?, &s("signature").ok_or("signature required")?, s("chain").as_deref()),
        "id.poll" => crate::siteid::poll(&s("nonce").ok_or("nonce required")?),
        "id.link_start" => crate::siteid::link_start(&s("key").ok_or("key required")?, &s("host").ok_or("host required")?, s("via").as_deref()),
        "id.box_put" => crate::sitesign::box_put(&s("box").ok_or("box required")?, &s("msg").ok_or("msg required")?),
        "id.box_take" => crate::sitesign::box_take(&s("box").ok_or("box required")?),
        "id.sign_pair" => crate::sitesign::pair(node, &s("qr").ok_or("qr required")?, req["send"] == true),
        "id.sign_sites" => crate::sitesign::sites(node),
        "id.sign_forget" => crate::sitesign::forget(node, &s("id").ok_or("id required")?),
        "id.sign_pending" => crate::sitesign::pending(),
        "id.sign_decide" => crate::sitesign::decide(node, &s("id").ok_or("id required")?, req["ok"] == true),
        "id.link_answer" => crate::siteid::link_answer(node, &s("qr").ok_or("qr required")?, req["send"] == true),
        "id.link_send" => crate::siteid::link_send(&s("code").ok_or("code required")?, &s("sealed").ok_or("sealed required")?),
        "id.link_poll" => crate::siteid::link_poll(&s("code").ok_or("code required")?),
        "id.answer" => crate::siteid::answer(node, &s("code").ok_or("code required")?, s("chain").as_deref(), req["send"] == true),
        "id.relay" => crate::siteid::relay(&s("uri").ok_or("uri required")?, &s("address").ok_or("address required")?, &s("signature").ok_or("signature required")?, s("chain").as_deref()),
        "vault.put" => crate::siteid::vault_put(node, req),
        "vault.get" => crate::siteid::vault_get(node, &s("tag").ok_or("tag required")?, req["here"] == true),
        "vault.delete" => crate::siteid::vault_delete(node, &s("tag").ok_or("tag required")?, &s("signature").ok_or("signature required")?),
        "tx.coins" => crate::sitesvc::coins(node, req),
        "tx.activity" => crate::sitesvc::activity(node, req),
        "tx.fee" => crate::sitesvc::fee(node, req),
        "tx.broadcast" => crate::sitesvc::broadcast(node, req),
        // Build-only transactions for a key that signs in the ARCADE app (crate::psbtbuild; ARCADE Git #23).
        "psbt.send" | "psbt.feed_post" | "psbt.feed_act" | "psbt.finalize" => crate::psbtbuild::dispatch(node, req),
        "msg.verify" => crate::psbtbuild::verify(req),
        "realtime.send" => crate::realtime::send(node, &s("token").ok_or("token required")?, req["data"].as_str(), req["b64"].as_str()),
        "realtime.leave" => crate::realtime::leave(node, &s("token").ok_or("token required")?),
        "realtime.poll" => crate::realtime::poll(&s("token").ok_or("token required")?, req["wait"].as_u64().unwrap_or(0)),
        "realtime.status" => Ok(crate::realtime::status(node)),
        "realtime.fast" => {
            let to: Vec<String> = req["to"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::realtime::fast::send(node, &s("token").ok_or("token required")?, req["data"].as_str(), req["b64"].as_str(), &to, req["reliable"] == true)
        }
        "realtime.media" => {
            let to: Vec<String> = req["to"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::realtime::fast::media(node, &s("token").ok_or("token required")?, &s("b64").ok_or("b64 required")?, &to)
        }
        "trade.leg" => crate::tradesvc::leg(node, &req["give"], &req["take"], &s("buyer").unwrap_or_default()),
        "trade.open" => crate::tradesvc::open(node, &s("sealed").unwrap_or_default()),
        "trade.fill" => crate::tradesvc::fill(node, &req["leg"], &req["expect"], &s("peer").unwrap_or_default(), req["send"] == true),
        "trade.items" => crate::tradesvc::items(node),
        "trade.watch" => Ok(crate::tradesvc::watch(node, &s("txid").unwrap_or_default(), s("inscription").as_deref(), s("owner").as_deref())),
        "trade.who" => crate::tradesvc::who(node, &s("address").unwrap_or_default()),
        "prize.seed" => crate::prizesvc::seed(node, &s("page").unwrap_or_default(), req),
        "prize.claim" => crate::prizesvc::claim(node, &s("page").unwrap_or_default(), req, req["send"] == true),
        "referee.get" => Ok(crate::refereesvc::setting(node)),
        "referee.set" => crate::refereesvc::set(node, &s("url").unwrap_or_default()),
        "collections.known_get" => Ok(serde_json::json!({"known": crate::knowncoll::enabled(node)})),
        "dogefeed.registrar" => crate::dproto::registrar(node, req),
        "dogefeed.stamp" => crate::dproto::stamp(node, req),
        "dogefeed.sign_action" => crate::dproto::sign_action(node, req),
        "dogefeed.post" => crate::dproto::post(node, req),
        "dogefeed.act" => crate::dproto::act(node, req),
        "dogefeed.registrars_add" | "dogefeed.registrars_remove" => crate::dproto::registrars_edit(node, req["cmd"].as_str().unwrap_or(""), req),
        "collections.known_set" => crate::knowncoll::set(node, req["on"] == true),
        "collections.list_add" | "collections.list_remove" | "collections.list_copy" => {
            let key = s("chain").ok_or("chain required")?;
            let p = crate::chains::find(&key).ok_or_else(|| format!("unknown chain {key}"))?;
            match req["cmd"].as_str().unwrap_or("") {
                "collections.list_add" => crate::oplist::add(node, p, req),
                "collections.list_remove" => crate::oplist::remove(node, p, &s("slug").ok_or("slug required")?),
                _ => crate::oplist::copy(node, p, &s("slug").ok_or("slug required")?),
            }
        }
        "realtime.routes" => Ok(crate::realtime::routes(s("origin").as_deref())),
        "realtime.lanes" => crate::realtime::fast::lanes(node, &s("token").ok_or("token required")?),
        "realtime.rtc" => crate::rtcsvc::api(node, &s("token").ok_or("token required")?, &s("peer").ok_or("peer required")?, &s("op").ok_or("op required")?, &req["signal"]),
        "realtime.rtc_status" => Ok(crate::rtcsvc::status()),
        "ring.send" => crate::ringsvc::send(node, &s("page").ok_or("page required")?, &s("to").unwrap_or_default(), &s("line").unwrap_or_default(), &s("link").unwrap_or_default()),
        "ring.waiting" => Ok(crate::ringsvc::waiting(node)),
        "ring.get" => Ok(crate::ringsvc::get(node, &s("id").ok_or("id required")?, &s("page").ok_or("page required")?)),
        "apps.node" => match s("op").as_deref().unwrap_or("") {
            "identity" => crate::msgsvc::node_identity(node),
            "send" => crate::msgsvc::node_send(node, &s("page").ok_or("page required")?, &s("to").unwrap_or_default(), &req["body"]),
            "sent" => Ok(json!({"sent": crate::msgsvc::node_sent(node, &s("page").ok_or("page required")?)})),
            "replies" => Ok(json!({"replies": crate::msgsvc::node_replies(node, &s("page").ok_or("page required")?, req["after"].as_u64().unwrap_or(0), req["limit"].as_u64().unwrap_or(100) as usize)})),
            _ => Err("node ops are identity, send, sent and replies".into()),
        },
        "apps.storage" => crate::appsvc::storage(node, &s("page").ok_or("page required")?, &s("op").ok_or("op required")?, req["key"].as_str(), req["value"].as_str()),
        "apps.launch" => crate::appsvc::launch(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?),
        // 2026-11-21 (ARCADE #13): optional filters; an empty string counts as none.
        "rpc.help" => Ok(crate::commands::help_for(s("method").filter(|m| !m.is_empty()).as_deref(), s("prefix").filter(|x| !x.is_empty()).as_deref())),
        "rpc.openapi" => Ok(crate::openapi::document_for(s("prefix").filter(|x| !x.is_empty()).as_deref())),
        "rpc.guide" => Ok(json!(crate::openapi::GUIDE)),
        "events.since" => events_since(node, req),
        "chains.wait" => crate::explorer::chain_wait(node, req),
        "apikey.list" => Ok(crate::apikeys::list(node)),
        "apikey.create" => {
            let perms: Vec<String> = req["perms"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            crate::apikeys::create(node, &s("name").ok_or("name required")?, &perms, req["tip_limit"].as_u64().unwrap_or(0))
        }
        "apikey.revoke" => crate::apikeys::revoke(node, &s("id").ok_or("id required")?),
        "rpc.info" => Ok(json!({"url": format!("http://127.0.0.1:{}/rpc", node.http_port.get().copied().unwrap_or(8420)), "api": format!("http://127.0.0.1:{}/api/v1/", node.http_port.get().copied().unwrap_or(8420)), "cookie_file": node.home.canonicalize().unwrap_or_else(|_| node.home.clone()).join(".cookie")})),
        "msg.delete" => crate::msgsvc::delete(node, &s("id").ok_or("id required")?),
        "msg.announce" => Ok(json!({"txid": crate::msgsvc::announce_if_needed(node)?})),
        "profile.publish_addresses" => crate::msgsvc::publish_addresses(node, req["send"] == true),
        "contacts.list" => Ok(crate::msgsvc::contacts_list(node)),
        "contacts.save" => crate::msgsvc::contact_save(node, &s("key").ok_or("key required")?, &s("name").unwrap_or_default(), &s("address").unwrap_or_default(), &s("notes").unwrap_or_default()),
        "contacts.delete" => crate::msgsvc::contact_delete(node, &s("key").ok_or("key required")?),
        "mute.list" => Ok(json!(crate::msgsvc::mutes(node))),
        "mute.add" => crate::msgsvc::mute(node, &s("address").ok_or("address required")?, true),
        "mute.remove" => crate::msgsvc::mute(node, &s("address").ok_or("address required")?, false),
        // `chain`: the home chain (default) or the ARCADE chain (§15 step 7).
        // `tag` may name its chain (`alice.doge`), or `chain` does; else where new tags go (ARCADE when usable).
        "tag.check" | "tag.claim" => {
            let raw = s("tag").ok_or("tag required")?;
            let (name, named) = crate::hometag::parse(node, &raw)?;
            let key = named.map(|p| p.key.to_string()).or(s("chain"));
            let p = crate::claims::chain_for(node, key.as_deref())?;
            if req["cmd"] == "tag.check" {
                let mut v = json!(crate::claims::check_on(node, p, &name));
                v["chain"] = json!(p.key);
                v["full"] = json!(crate::hometag::qualified(p, &name));
                v["label"] = json!(crate::hometag::label(p, &name));
                Ok(v)
            } else {
                Ok(json!(crate::claims::start_on(node, p, &name)?))
            }
        }
        // What an @tag names (tags per chain): one holder, or each chain's when different people hold it (a picker).
        "tag.resolve" => {
            let raw = s("tag").ok_or("tag required")?;
            let (name, _) = crate::hometag::parse(node, &raw)?;
            Ok(match crate::hometag::find(node, &raw)? {
                crate::hometag::Found::One(p, a) => json!({"found": "one", "choices": crate::hometag::choices(node, &name, &[(p, a)])}),
                crate::hometag::Found::Many(rows) => json!({"found": "many", "choices": crate::hometag::choices(node, &name, &rows), "note": crate::hometag::which_text(&name, &rows)}),
                crate::hometag::Found::None => json!({"found": "none", "choices": []}),
            })
        }
        // The ledger on the ARCADE chain (§15 step 7).
        "ledger.announce" => crate::arcledger::announce(node),
        "ledger.profile" => crate::arcledger::profile(node, &req),
        "ledger.repo_push" => crate::arcledger::repo_push(node, &s("name").ok_or("name required")?, &s("manifest").ok_or("manifest required")?, s("base").as_deref(), req["force"] == true),
        "ledger.repo_maintainer" => crate::arcledger::repo_maintainer(node, &s("name").ok_or("name required")?, &s("tag").ok_or("tag required")?, req["add"] != false),
        "ledger.repo_fork" => crate::arcledger::repo_fork(node, &s("from").ok_or("from required")?, &s("name").ok_or("name required")?),
        "ledger.item_open" => crate::arcledger::item_open(node, &req),
        "ledger.item_comment" => crate::arcledger::item_comment(node, &req),
        "ledger.item_state" => crate::arcledger::item_state(node, &req),
        "ledger.items" => {
            let mut v = crate::arcledger::items(node, &req)?;
            if req["pending"] != false {
                crate::arcledger::add_pending(node, &mut v);
            }
            page_items(filter_items(v, req)?, req)
        }
        "ledger.repo" => crate::arcledger::repo(node, &s("name").ok_or("name required")?),
        "ledger.repos" => crate::arcledger::repos(node, s("owner").as_deref()),
        // A:// (profiles, follows, the address book, posts) on the ARCADE chain.
        "a.profile" => crate::arcledger::a_profile(node, &req),
        "a.profile_set" => crate::arcledger::a_profile_set(node, &req),
        "a.follow" => crate::arcledger::a_follow(node, &s("tag").ok_or("tag required")?, true),
        "a.unfollow" => crate::arcledger::a_follow(node, &s("tag").ok_or("tag required")?, false),
        "a.follows" => crate::arcledger::a_follows(node, &req),
        "a.contacts" => crate::arcledger::a_contacts(node),
        "a.contact_save" => crate::arcledger::a_contact_save(node, &req),
        "a.search" => crate::arcledger::a_search(node, &s("q").unwrap_or_default()),
        "a.contact_add" => crate::arcledger::a_contact_add(node, &req),
        "a.contact_delete" => crate::arcledger::a_contact_delete(node, &s("id").ok_or("id required")?),
        "a.wallets" => crate::walletproofs::a_wallets(node, &req),
        "a.wallets_publish" => crate::walletproofs::publish(node, req["force"] == true),
        "a.card_addresses" => crate::walletproofs::card_addresses(node),
        "a.card_address" => crate::walletproofs::set_card_address(node, &s("coin").ok_or("coin required")?, &s("address").ok_or("address required")?, req["show"] != false),
        "a.post" => crate::arcledger::a_post(node, &s("text").ok_or("text required")?),
        "a.reactions" => crate::asocial::reactions(node, &req),
        "a.replies" => crate::asocial::replies(node, &req),
        "a.react" => crate::asocial::react(node, &req),
        "a.reply" => crate::asocial::reply(node, &req),
        "a.tip" => crate::asocial::tip(node, &req),
        "a.posts" => crate::arcledger::a_posts(node, &req),
        "ledger.balances" => crate::arcledger::balances(node, &s("address").ok_or("address required")?),
        "ledger.tag_commit" => crate::arcledger::tag_commit(node, &s("tag").ok_or("tag required")?),
        "ledger.tag_checkin" => crate::arcledger::tag_checkin(node),
        "ledger.send" => {
            let payload = hex::decode(s("hex").ok_or("hex required (the AnyData payload)")?.trim()).map_err(|_| "hex is not hex")?;
            let (txid, fee) = crate::arcledger::send(node, &payload)?;
            Ok(json!({"txid": txid, "fee": fee}))
        }
        "tag.list" => Ok(json!(node.app.claims().map_err(|e| e.to_string())?)),
        "start.status" => Ok(crate::claims::start_status(node)),
        "faucet.ask" => {
            let (k, _) = node.app.tag_key(p, s("index").and_then(|i| i.parse().ok()).unwrap_or(0)).map_err(|e| e.to_string())?;
            Ok(json!(crate::faucet::ask(node, &k.address, &s("tag").unwrap_or_default())?))
        }
        "faucet.arc" => {
            // A website signs a user in and names their ARCADE address; without one, this wallet's.
            // (The user faucet: for a website's users only. An arcade's own wallet has the node faucet.)
            match s("address").filter(|a| !a.is_empty()) {
                Some(a) => crate::arcfaucet::ask(node, &a),
                None => Err("faucet.arc tops up a website user's ARCADE address: name it (address). This arcade's own wallet gets its ARCADE from the node faucet (faucet.arc_status)".into()),
            }
        }
        "faucet.arc_status" => Ok(crate::arcfaucet::status(node)),
        "faucet.newcomers" => Ok(crate::arcfaucet::newcomers(node)),
        "faucet.welcome" => crate::arcfaucet::welcome(node, &s("node").ok_or("give the new arcade's node")?, req.get("amount")),
        "faucet.welcome_set" => crate::arcfaucet::welcome_set(node, req),
        "mempool" => {
            // The chain asked for (`--chain`), the home chain by default; the ARCADE chain's pool is its own.
            let (p, borrowed) = crate::sitesvc::chain_or_borrowed(node, req)?;
            if borrowed && !crate::arcsvc::defined().is_some_and(|d| d.chain_no == p.chain_no) {
                // Borrowed: the lenders' pool (the Explorer's), plus what this arcade sent that they don't list yet.
                let got = crate::explorer::handle(node, "explorer.mempool", &json!({"chain": p.key})).unwrap_or_else(|_| json!({"txs": []}));
                let mut txs: Vec<Value> = got["txs"].as_array().cloned().unwrap_or_default().into_iter()
                    .map(|t| json!({"txid": t["txid"], "seen": t["seen"], "size": t["size"], "outputs": t["out"]})).collect();
                for t in crate::borrow::pool_txs(p) {
                    if !txs.iter().any(|x| x["txid"] == t.txid.to_hex().as_str()) {
                        txs.push(json!({"txid": t.txid.to_hex(), "seen": 0, "size": t.raw.len(), "outputs": t.outputs.iter().map(|o| o.value).sum::<u64>()}));
                    }
                }
                return Ok(json!({"count": txs.len(), "txs": txs, "pending_tags": crate::claims::pending_tags(node), "borrowed": true}));
            }
            let pool = if crate::arcsvc::borrowed(p) {
                // Borrowed: what this arcade sent that still waits at the lenders.
                crate::borrow::pool_txs(p).into_iter().map(|t| (0, t.raw)).collect()
            } else if crate::arcsvc::runs(p) {
                crate::arcsvc::waiting(5_000).into_iter().map(|raw| (0, raw)).collect()
            } else {
                node.chain.mempool(p.chain_no).map_err(|e| e.to_string())?
            };
            let txs: Vec<Value> = pool
                .iter()
                .filter_map(|(seen, raw)| arcade_chain::Tx::parse_with(raw, &p.format).ok().map(|t| (seen, t)))
                .map(|(seen, t)| json!({"txid": t.txid.to_hex(), "seen": seen, "size": t.raw.len(), "outputs": t.outputs.iter().map(|o| o.value).sum::<u64>()}))
                .collect();
            Ok(json!({"count": txs.len(), "txs": txs, "pending_tags": crate::claims::pending_tags(node)}))
        }
        "mine.start" => {
            use std::sync::atomic::Ordering;
            if !node.app.has_wallet().map_err(|e| e.to_string())? {
                return Err("create a wallet first (arcade wallet create)".into());
            }
            if !crate::homeinst::installed() {
                return Err(format!("{} is borrowed here, not installed: install it in Chains & Coins to mine it", node.params.name));
            }
            // Unless asked for more: the cores the CPU level allows (crate::cpu; half by default).
            let threads = s("threads").and_then(|t| t.parse().ok()).unwrap_or(crate::cpu::cores() as u64);
            if node.miner.running.swap(true, Ordering::Relaxed) {
                return Ok(json!({"running": true, "note": "already mining"}));
            }
            node.miner.threads.store(threads, Ordering::Relaxed);
            node.miner.hashes.store(0, Ordering::Relaxed);
            let n = node.self_arc();
            std::thread::Builder::new().name("miner".into()).spawn(move || crate::miner::run(n)).unwrap();
            // Started either way; it mines only at the tip and says why it waits (mine.status).
            Ok(json!({"running": true, "threads": threads, "waiting": crate::miner::not_at_tip(node)}))
        }
        "mine.stop" => {
            node.miner.running.store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(json!({"running": false}))
        }
        "stake.status" => Ok(crate::staker::status(node)),
        "stake.set" => crate::staker::set_on(node, req["on"] == true),
        "stake.check" => Ok(crate::staker::ask_check()),
        "mine.status" => {
            use std::sync::atomic::Ordering;
            let secs = node.miner.started.lock().unwrap().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            let h = node.miner.hashes.load(Ordering::Relaxed);
            Ok(json!({
                "running": node.miner.running.load(Ordering::Relaxed),
                "waiting": node.miner.waiting.lock().unwrap().clone(),
                "threads": node.miner.threads.load(Ordering::Relaxed),
                "hashes": h,
                "hash_rate": if secs > 0.0 { (h as f64 / secs).round() } else { 0.0 },
                "found": node.miner.found.load(Ordering::Relaxed),
            }))
        }
        "ledger" => {
            // Our ledger fingerprints next to what the existing arcades report.
            use arcade_ledger::engine;
            let floor = engine::gates(p).activation;
            let ours_h = node.chain.indexed_to(c, "ledger.v1").ok().flatten().unwrap_or(0);
            let tips = node.mesh_tips.lock().unwrap().clone();
            let mut peers = Vec::new();
            for (addr, t) in &tips {
                let mut v = json!({"peer": addr, "height": t.height, "at": t.at});
                if let Some(d) = &t.d {
                    if t.at as u32 >= floor && t.at as u32 <= ours_h.saturating_sub(6) {
                        let ours = engine::range_digest(&node.chain, c, floor, t.at as u32).unwrap_or_default();
                        v["d_match"] = json!(ours == *d);
                        v["d_ours"] = json!(ours);
                        v["d_theirs"] = json!(d);
                    }
                }
                if let Some(sv) = &t.s {
                    v["s_theirs"] = json!(sv);
                    v["s_same_height"] = json!(t.height as u32 == ours_h);
                }
                peers.push(v);
            }
            let h = s("height").and_then(|x| x.parse::<u32>().ok());
            Ok(json!({
                "indexed_to": ours_h,
                "checkpoint": engine::checkpoint(ours_h, floor),
                "consensus_hash": engine::consensus_hash(&node.chain, c).unwrap_or_default(),
                "names_digest": arcade_ledger::index::names_digest(&node.chain, c).unwrap_or_default(),
                "peers": peers,
                "block": h.map(|h| json!({"height": h, "digest": engine::block_digest(&node.chain, c, h).ok().flatten(), "rows": engine::block_records(&node.chain, c, h).unwrap_or_default()})),
            }))
        }
        "profile" => {
            use arcade_ledger::index;
            // Balance and history on the chain asked for (`--chain`, the ARCADE chain too), the home chain by default.
            // Tags and profiles: that chain's own ledger first (the ARCADE chain keeps one), else the home chain's.
            // (Borrowed too: balance and history from arcades keeping it, the profile from lent rows.)
            let (pc, pc_borrowed) = crate::sitesvc::chain_or_borrowed(node, req)?;
            let ledgers: Vec<u16> = if pc.chain_no == c { vec![c] } else { vec![pc.chain_no, c] };
            let q = s("who").ok_or("give an @tag or an address")?;
            let address = match q.strip_prefix('@') {
                Some(t) => {
                    // Tags per chain (Martin 2026-11-18): `@alice.doge`, or a plain `@alice` held on one chain (else the
                    // answer says which ones there are); read from lenders when their tag tables are borrowed here.
                    struct Row { address: String }
                    let (l, row) = crate::hometag::owner_or_err(node, t).map(|(hp, a)| (hp.chain_no, Row { address: a }))?;
                    // Claimed on another chain than the one asked about: the same key, written as that chain's address
                    // (as a send to @name finds it there).
                    match (l != pc.chain_no).then(|| arcade_chain::params::ALL_CHAINS.iter().copied().chain(crate::arcsvc::defined()).find(|x| x.chain_no == l)).flatten() {
                        Some(lp) => arcade_ledger::address::script_for_address(lp, &row.address)
                            .filter(|sc| sc.len() == 25 && sc[..3] == [0x76, 0xa9, 0x14])
                            .map(|sc| arcade_ledger::address::base58check(pc.p2pkh_version, &sc[3..23]))
                            .unwrap_or(row.address),
                        None => row.address,
                    }
                }
                None => q.clone(),
            };
            let tag = ledgers.iter().find_map(|&l| index::tag_of(&node.chain, l, &address).ok().flatten())
                .or_else(|| crate::hometag::of_address(node, &address));
            let mut prof = ledgers.iter().find_map(|&l| index::profile(&node.chain, l, &address).ok().flatten())
                .or_else(|| crate::msgsvc::profile_on(node, node.ledger_home(), &address));
            // A chain this arcade did not know when the profile was indexed is kept as "h160:<hex>": its address is
            // read now (2026-11-12: Bellscoin and Luckycoin showed as hashes, so nobody could pay them there).
            if let Some(list) = prof.as_mut().and_then(|v| v["linked"].as_array_mut()) {
                for l in list {
                    let (Some(net), Some(h)) = (l["chain"].as_str(), l["address"].as_str().and_then(|a| a.strip_prefix("h160:"))) else { continue };
                    if let (Some(np), Ok(b)) = (arcade_chain::params::by_key(net), hex::decode(h)) {
                        if b.len() == 20 {
                            l["address"] = json!(arcade_ledger::address::base58check(np.p2pkh_version, &b));
                        }
                    }
                }
            }
            // Who follows this account and whom it follows (follows.rs), as counts.
            let follow_counts = json!({"followers": crate::follows::followers_of(node, &address).len(), "following": crate::follows::following_of(node, &address).len()});
            let script = arcade_ledger::address::script_for_address(pc, &address);
            let (balance, history) = match &script {
                Some(_) if pc_borrowed => {
                    let v = crate::explorer::handle(node, "explorer.address", &json!({"chain": pc.key, "address": address, "inscriptions": false})).unwrap_or(Value::Null);
                    (v["balance"].as_u64().unwrap_or(0), v["tx_count"].as_u64().unwrap_or(0) as usize)
                }
                Some(sc) => (node.chain.balance_for_script(pc.chain_no, sc).unwrap_or(0), node.chain.history_count_for_script(pc.chain_no, sc).unwrap_or(0)),
                None => (0, 0),
            };
            // Does someone else's profile state this address as theirs?
            let linked_by = index::owner_of_linked(&node.chain, c, pc.key, &address).unwrap_or(None);
            // The tag in a profile is only a claim; verified when the tag table agrees.
            let tag_verified = match (prof.as_ref().and_then(|v| v["tag"].as_str()), &tag) {
                (Some(t), Some(real)) => Some(t == real),
                (Some(_), None) => Some(false),
                _ => None,
            };
            // Every tag the address's key holds, each with its chain (tags per chain).
            let tags: Vec<Value> = crate::hometag::all_of_address(node, &address).into_iter()
                .map(|(tp, t)| json!({"tag": crate::hometag::qualified(tp, &t), "name": t, "chain": tp.key, "label": crate::hometag::label(tp, &t)})).collect();
            Ok(json!({"chain": pc.key, "address": address, "tag": tag, "tags": tags, "balance": balance, "history_rows": history,
                      "profile": prof, "profile_tag_verified": tag_verified, "stated_by": linked_by, "follows": follow_counts}))
        }
        "ord" | "ord.chunks" | "name" | "map" | "token" | "tokens" => {
            let cp = match s("chain") {
                Some(k) => arcade_chain::params::by_key(&k).ok_or_else(|| format!("unknown chain {k}"))?,
                None => p,
            };
            let cc = cp.chain_no;
            let need = |ix: &str| -> Result<(), String> {
                let on = if cp.chain_no == p.chain_no { crate::chains::home_indexes(node) } else {
                    node.extra.lock().unwrap().get(cp.key).map(|r| r.indexes.clone()).unwrap_or_default()
                };
                if on.iter().any(|i| i == ix) { Ok(()) } else {
                    Err(format!("the {ix} index is not on {}; add it with  arcade chain index add {} {ix}", cp.name, cp.key))
                }
            };
            use arcade_ledger::{maps, names, ord, tokens};
            let cmd = req["cmd"].as_str().unwrap();
            let local = (|| match cmd {
                "ord" => {
                    need("ord")?;
                    match s("id") {
                        Some(id) => ord::inscription(&node.chain, cp, &id).ok_or_else(|| format!("no inscription {id} on {}", cp.name)),
                        None => Ok(json!({"chain": cp.key, "inscriptions": ord::count(&node.chain, cc),
                                          "indexed_to": node.chain.indexed_to(cc, "ord.v1").ok().flatten(), "start": ord::ord_start(cp)})),
                    }
                }
                "ord.chunks" => {
                    need("ord")?;
                    let from = req["from"].as_u64().unwrap_or(0) as u32;
                    let n = req["n"].as_u64().unwrap_or(10).min(500) as u32;
                    Ok(json!((from..from + n).filter_map(|i| ord::chunk(&node.chain, cp, i).map(|(h, b)| json!([i, h, b]))).collect::<Vec<_>>()))
                }
                "name" => {
                    need("names")?;
                    let q = s("name").ok_or("give a name")?;
                    let wins = names::lookup(&node.chain, cc, &q);
                    Ok(json!({"name": q, "readings": wins.iter().map(|(rule, n, id)| json!({"rule": rule, "number": n, "id": id,
                        "owner": ord::inscription(&node.chain, cp, id).and_then(|v| v.get("owner").cloned())})).collect::<Vec<_>>(),
                        "counts": names::counts(&node.chain, cc)}))
                }
                "map" => {
                    need("maps")?;
                    match s("claim") {
                        Some(q) => {
                            let (n, id) = maps::lookup(&node.chain, cc, &q).ok_or_else(|| format!("{q} is not claimed"))?;
                            Ok(json!({"claim": q, "number": n, "id": id, "owner": ord::inscription(&node.chain, cp, &id).and_then(|v| v.get("owner").cloned())}))
                        }
                        None => Ok(json!(maps::counts(&node.chain, cc))),
                    }
                }
                "token" => {
                    need("tokens")?;
                    match s("tick") {
                        Some(t) => tokens::tick(&node.chain, cc, &t).ok_or_else(|| format!("no token {t}")),
                        None => Ok(json!({"ticks": tokens::tick_count(&node.chain, cc)})),
                    }
                }
                _ => {
                    need("tokens")?;
                    let a = s("owner").ok_or("give an address")?;
                    Ok(json!(tokens::balances(&node.chain, cc, &a).into_iter().map(|(t, o, tr)| json!({"tick": t, "overall": o, "transferable": tr})).collect::<Vec<_>>()))
                }
            })();
            // Borrowed = installed (batch 5): the index not added here, or the chain only borrowed here (where a claim
            // newer than our copy isn't found yet): read from an arcade keeping the index. (ord.chunks are this
            // arcade's own fingerprints, never asked elsewhere.)
            match local {
                Err(e) if cmd != "ord.chunks" && (crate::explorer::names_missing_index(&e) || crate::borrow::is_borrowed(cp.key)) => {
                    crate::explorer::index_read_elsewhere(node, cmd, cp, &req).map_err(|why| format!("{e} (asked the mesh too: {why})"))
                }
                r => r,
            }
        }
        // This arcade's ARCADE pay and Earn progress, read only (arc.earn without the switch).
        "explorer.earn" => crate::arcsvc::earn_read(node, &req),
        c if c.starts_with("explorer.") => crate::explorer::handle_with(node, c, &req, req["img"] == "links"),
        c @ ("ledger.book" | "ledger.trades" | "ledger.pairs" | "market.holdings") => crate::marketsvc::handle(node, c, &req),
        // Token sends from this wallet (tokensend.rs): arcade tokens in one transaction; PRC-20 / DRC-20 as a job.
        "token.send" => crate::tokensend::ledger_send(node, req["propertyid"].as_u64().or_else(|| s("propertyid").and_then(|x| x.parse().ok())).ok_or("propertyid required")? as u32,
            req["units"].as_u64().or_else(|| s("units").and_then(|x| x.parse().ok())).ok_or("units required (base units)")?, &s("to").ok_or("to required")?, req["send"] == true, s("chain").as_deref()),
        "token20.send" => crate::tokensend::t20_send(node, &s("chain").ok_or("chain required")?, &s("tick").ok_or("tick required")?, &s("amount").ok_or("amount required")?, &s("to").ok_or("to required")?, req["send"] == true),
        "token20.sends" => Ok(crate::tokensend::t20_sends(node)),
        "ord.send" => crate::ordsend::send(node, s("chain").as_deref(), &s("inscription").ok_or("inscription required")?, &s("to").ok_or("to required")?, req["send"] == true),
        "swap.place" => crate::swaps::place(node, &req["give"], &req["get"], &req),
        "swap.release" => crate::swaps::release(node, &s("order").ok_or("order required")?),
        "swap.book" => Ok(json!(crate::swaps::book(node))),
        // With the legs shown ({give, get}), only on exactly those terms.
        "swap.take" => crate::swaps::take(node, &s("node").ok_or("node required")?, &s("order").ok_or("order required")?, req["give"].is_object().then_some(req)),
        "swap.list" => Ok(json!(crate::swaps::swaps(node))),
        c @ ("swap.quote" | "swap.told" | "swap.status" | "swap.letgo") => crate::swaps::outside(node, &c[5..], &req),
        "chains.offers" => Ok(json!(node.offers.lock().unwrap().iter().map(|(id, o)| json!({"node": id, "addr": o.addr, "at": o.at, "bk": o.bk, "cr": o.cr})).collect::<Vec<_>>())),
        "chains.peers" => {
            let p = arcade_chain::params::by_key(&s("key").ok_or("key required")?).ok_or("unknown chain")?;
            let (known, good, resting) = crate::peerbook::summary(node, p.net);
            Ok(json!({"chain": p.key, "known": known, "working": good, "resting": resting, "good": crate::peerbook::good(node, p.net, 50), "next": node.coin_candidates(p).into_iter().take(10).collect::<Vec<_>>()}))
        }
        // From a copy made in the background: never held up by a slow chain.
        "chains.list" => Ok(crate::chains::list_cached(node)),
        "mesh.overview" => Ok(crate::meshview::overview(node)),
        "chains.sizes" => {
            // Measure now (instead of waiting for the six-hourly pass) and show the result.
            let m = crate::sizes::measure(node)?;
            *node.my_sizes.lock().unwrap() = m.clone();
            Ok(json!({"measured": m, "chains": crate::chains::list(node)}))
        }
        "chains.status" => {
            let key = s("key").ok_or("key required")?;
            let mut st = crate::chains::status(node, &key)?;
            // Where its file is, and a move to another drive (crate::chaindrive).
            if let Some(p) = crate::chains::find(&key).filter(|_| st["installed"] != false && st.is_object()) {
                st["place"] = crate::chaindrive::place(node, p.chain_no);
                st["move"] = crate::chaindrive::status(p.key);
            }
            // A tip wound back when this arcade started (the last stop left without writing the coin
            // caches): the stretch between is connected again from the stored blocks.
            if st.is_object() {
                let no = crate::chains::find(&key)
                    .map(|p| p.chain_no)
                    .or_else(|| arcade_chain::params::ARCADE_CHAINS.iter().find(|a| a.1 == key).map(|a| a.0));
                st["recovered_at_start"] = match no.and_then(|c| node.chain.recovered_at_start(c)) {
                    Some((from, to)) => json!({"from": from, "to": to}),
                    None => Value::Null,
                };
            }
            Ok(st)
        }
        "chains.drives" => Ok(crate::chaindrive::drives(node)),
        "chains.move" => crate::chaindrive::start(&node.self_arc(), &s("key").ok_or("key required")?, &s("drive").unwrap_or_else(|| "home".into())),
        "arc.status" => crate::arcsvc::status(node),
        "tx.status" => crate::receipts::status(node, req),
        "tx.wait" => crate::receipts::wait(node, req),
        "tx.resend" => crate::ownwait::api_resend(node, req),
        "arc.earning" => crate::arcsvc::earning(node),
        "arc.block" => crate::arcsvc::block(req),
        "arc.submit" => crate::arcsvc::submit(req),
        "arc.pool" => crate::arcsvc::pool(req),
        "arc.catalog" => crate::arcsvc::catalog(node),
        "arc.segment_root" => crate::arcsvc::segment_root(node, req),
        "arc.seal" => crate::arcsvc::seal(&node.self_arc(), req),
        "arc.replicas" => crate::arcsvc::replicas(node),
        // A reveal demand by hand (§2.4): the holders of a listed segment must show its leaves on the chain.
        "arc.reveal" => {
            let svc = crate::arcsvc::svc().ok_or("the ARCADE chain isn't running here")?;
            let seg: [u8; 32] = s("segment").and_then(|x| hex::decode(x).ok()).and_then(|b| b.try_into().ok()).ok_or("segment required (64 hex)")?;
            let first = req["first"].as_u64().unwrap_or(0);
            let count = req["count"].as_u64().unwrap_or(16);
            let id = crate::arcfraud::demand(node, &svc.net(), &seg, first, count)?;
            Ok(serde_json::json!({"demand": hex::encode(id), "segment": hex::encode(seg), "first": first, "count": count}))
        }
        // A serving check by hand (§5.4), no verdict posted: does `owner` hand out these blocks?
        "arc.serve_check" => {
            let owner: [u8; 32] = s("owner").and_then(|x| hex::decode(x).ok()).and_then(|b| b.try_into().ok()).ok_or("owner required (64 hex)")?;
            let p = s("chain").and_then(|k| arcade_chain::params::by_key(&k)).ok_or("chain required (a coin key)")?;
            let hashes: Vec<arcade_chain::Hash256> = match req["hashes"].as_array() {
                Some(a) => a.iter().filter_map(|x| x.as_str().and_then(|s| arcade_chain::Hash256::from_hex(s).ok())).collect(),
                None => {
                    let first = req["first"].as_u64().ok_or("first and count, or hashes")?;
                    let count = req["count"].as_u64().unwrap_or(50).min(2_000);
                    (first..first + count).map_while(|h| node.chain.hash_at(p.chain_no, h as u32).ok().flatten()).collect()
                }
            };
            if hashes.is_empty() {
                return Err("no blocks to ask for (this arcade doesn't keep those heights)".into());
            }
            let t = std::time::Instant::now();
            let how = if crate::crvia::linked(&hex::encode(owner)) { "its mesh link" } else { "dialing it" };
            let outcome = crate::arcserve::check(node, p, &owner, &hashes);
            Ok(serde_json::json!({"blocks": hashes.len(), "via": how, "outcome": format!("{outcome:?}"), "seconds": t.elapsed().as_secs_f64()}))
        }
        "arc.keeper" => crate::arckeep::api(node, req),
        "arc.genesis" => crate::arcgenesis::status(node),
        "arc.blob" => crate::arcsvc::blob(req),
        // Fog storage (rules version 5, crate::fogstore): the share kept by choice, and what the fog keeps here.
        "arc.keep" => crate::fogstore::api_keep(node, req),
        "arc.fog" => Ok(crate::fogstore::status()),
        // What this arcade keeps by its operator's choice, and forgets (crate::fogkeep; the Storage app's Mine tab).
        "fog.keep" => crate::fogkeep::api(node, "keep", req),
        "fog.forget" => crate::fogkeep::api(node, "forget", req),
        "fog.undo" => crate::fogkeep::api(node, "undo", req),
        "fog.mine" => crate::fogkeep::mine(node),
        // The viewer cache (§15 step 4): its size in MB (0: off; on by default at 2,048) and what it holds.
        "arc.cache" => {
            if let Some(mb) = req["mb"].as_u64() {
                node.app.store.write(|w| w.put(arcade_app::META, b"arc_cache_mb", mb.to_string().as_bytes())).map_err(|e| e.to_string())?;
                crate::arcblob::set_cache_limit(mb << 20);
            }
            let (files, bytes) = crate::arcblob::cache_use();
            Ok(serde_json::json!({"mb": crate::arcblob::cache_limit() >> 20, "on": crate::arcblob::cache_limit() > 0, "files": files, "bytes": bytes}))
        }
        // Several blobs in one transaction: `files` [{path, type, class?, gzip?}], `send`.
        "arc.blob_publish_many" => {
            let mut items = Vec::new();
            for f in req["files"].as_array().ok_or("files required")? {
                // The bytes as hex (any surface), or a file on this computer (the socket and the cookie RPC).
                let bytes = match (f["hex"].as_str(), f["path"].as_str()) {
                    (Some(h), _) => hex::decode(h.trim()).map_err(|_| "a file's hex is not hex".to_string())?,
                    (None, Some(path)) => std::fs::read(path).map_err(|e| format!("{path}: {e}"))?,
                    _ => return Err("each file needs its hex (or its path on this computer)".into()),
                };
                let class = arcade_arc::blob::Class::from_u8(f["class"].as_u64().unwrap_or(0) as u8).ok_or("class is 0 to 3")?;
                let enc = if f["gzip"] == true { arcade_arc::blob::Encoding::Gzip } else { arcade_arc::blob::Encoding::Plain };
                items.push((bytes, f["type"].as_str().unwrap_or("application/octet-stream").to_string(), class, enc));
            }
            crate::coinsend::publish_blobs(node, &items, req["send"] == true)
        }
        // Keep every blob's plain copy (an archive node), or let the classes drop them (§15 step 3).
        "arc.archive" => {
            if let Some(on) = req["on"].as_bool() {
                node.app.store.write(|w| w.put(arcade_app::META, b"arc_archive", if on { b"1" } else { b"0" })).map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({"archive": crate::arcsvc::archive(node)}))
        }
        // A blob from this arcade's wallet: `path` (a file here) or `hex`, its `type`, `class` (0 permanent, 1 release,
        // 2 temporary), `gzip`; `send` to put it in the pool (without: only what it would cost).
        // A large blob (crate::largeblob, up to 100 MB): parts over several blocks, then its manifest.
        "arc.blob_publish_large" => crate::largeblob::api_publish(node, req),
        "arc.blob_publish_status" => crate::largeblob::api_status(req),
        // Rules version 6: a Temporary blob made Permanent for the difference (`id`, `send`; without send, the quote).
        "arc.blob_upgrade" => {
            let id: arcade_arc::merkle::Hash = s("id").and_then(|x| hex::decode(x.trim()).ok()).and_then(|b| b.try_into().ok()).ok_or("id required (the blob's id, 64 hex)")?;
            crate::coinsend::upgrade_blob(node, &id, req["send"] == true)
        }
        "arc.blob_publish" => {
            // Over one block's 4 MB: a large blob.
            let big = match (s("path"), s("hex")) {
                (Some(path), _) => std::fs::metadata(&path).map(|m| m.len() > arcade_arc::blob::MAX_BLOB_BYTES as u64).unwrap_or(false),
                (None, Some(h)) => h.trim().len() / 2 > arcade_arc::blob::MAX_BLOB_BYTES,
                _ => false,
            };
            if big {
                return crate::largeblob::api_publish(node, req);
            }
            let bytes = match (s("path"), s("hex")) {
                (Some(path), _) => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                (None, Some(h)) => hex::decode(h.trim()).map_err(|_| "hex is not hex".to_string())?,
                _ => return Err("path or hex required".into()),
            };
            let class = arcade_arc::blob::Class::from_u8(req["class"].as_u64().unwrap_or(0) as u8).ok_or("class is 0, 1 or 2")?;
            let enc = if req["gzip"] == true { arcade_arc::blob::Encoding::Gzip } else { arcade_arc::blob::Encoding::Plain };
            crate::coinsend::publish_blob(node, &bytes, &s("type").unwrap_or_else(|| "application/octet-stream".into()), class, enc, req["send"] == true)
        }
        "code.status" => Ok(crate::arccode::status()),
        "code.add" => crate::arccode::add(req),
        "code.export" => crate::arccode::export(req),
        "arc.open" => crate::arcsvc::open(node, req),
        "arc.sealing" => crate::arcsvc::sealing(node, req),
        "arc.earn" => crate::arcsvc::earn(node, req),
        "chains.add" => {
            let with: Vec<String> = req["with"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let key = s("key").ok_or("key required")?;
            let mode = s("mode").unwrap_or_else(|| "full".into());
            // init: headers agreed by several sources, blocks stored only, then built in bulk
            // (crate::initsync, docs/INITSYNC.md). Remembered until the install starts. Not said:
            // init wherever the chain allows it (crate::initsync::choose); false: a plain install.
            // core_dir: the operator already runs this chain's Core on this computer; its folder
            // is read in place when the install starts (crate::coreboot::request_dir), not init.
            let core_dir = s("core_dir").filter(|d| !d.trim().is_empty());
            if core_dir.is_some() && req["init"] == true {
                return Err("init or your own Core's folder, not both".into());
            }
            if core_dir.is_some() && !matches!(mode.as_str(), "full" | "chain") {
                return Err("reading your Core's folder needs every block kept: mode full or chain".into());
            }
            let asked = if core_dir.is_some() { Some(false) } else { req["init"].as_bool() };
            let canon = arcade_chain::params::by_key(&key).map(|p| p.key.to_string()).unwrap_or_else(|| key.clone());
            let param = arcade_chain::params::by_key(&canon);
            // The ARCADE chain and the home chain have installs of their own (crate::chains::add).
            let own_install = crate::arcsvc::defines(&canon) || param.is_some_and(|p| p.chain_no == node.params.chain_no);
            let fresh = node.chain_params(&canon).is_none();
            let allowed = match param {
                Some(_) if own_install => Err("this chain installs its own way".to_string()),
                Some(p) => crate::initsync::check(p, &mode),
                None => Err(format!("{key}: no such chain")),
            };
            if asked == Some(true) && param.is_none() {
                return Err(format!("{key}: no such chain"));
            }
            let (init, why_not) = crate::initsync::choose(asked, fresh, allowed, crate::initsync::default_on()).map_err(|e| format!("{canon}: {e}"))?;
            if fresh && !own_install {
                // Also turns off init (or a Core folder) asked for earlier, when this add says otherwise.
                crate::coreboot::request_dir(node, &canon, core_dir.as_deref())?;
                crate::initsync::request(node, &canon, init);
            } else if core_dir.is_some() && own_install {
                return Err(format!("{canon} installs its own way, not from a Core folder"));
            } else if core_dir.is_some() {
                return Err(format!("{canon} is installed already; read a Core folder into it with: arcade chain copy start {canon} --from-dir <path>"));
            }
            // The drive chosen for it, else this arcade's own (crate::chaindrive): its file starts there.
            let r = crate::chaindrive::add(node, &key, &s("drive").unwrap_or_default(), || crate::chains::add(&node.self_arc(), &key, &mode, with));
            if r.is_err() && fresh && !own_install {
                crate::initsync::request(node, &canon, false);
                let _ = crate::coreboot::request_dir(node, &canon, None);
            }
            r.map(|mut v| {
                if fresh && !own_install {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("init_install".into(), json!(init));
                        if let Some(d) = &core_dir {
                            o.insert("core_dir".into(), json!(d));
                        }
                        if let Some(w) = why_not {
                            let w = w.replace(" (install it without --init)", "").replace(": install it without --init", "");
                            o.insert("init_why_not".into(), json!(w));
                        }
                    }
                }
                v
            })
        }
        "chains.queue" => Ok(crate::chains::queue_list(node)),
        "chains.unqueue" => crate::chains::unqueue(node, &s("key").ok_or("key required")?),
        "chains.queue_first" => crate::chains::queue_first(node, &s("key").ok_or("key required")?),
        "chains.start_now" => crate::chains::start_now(&node.self_arc(), &s("key").ok_or("key required")?),
        "chains.hide" => crate::chains::hide(node, &s("key").ok_or("key required")?, req["hidden"].as_bool().unwrap_or(true)),
        "chains.plan" => {
            let keys: Vec<String> = req["keys"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            // core_dirs: {key: folder} for chains the operator already runs a Core for (the chooser asks).
            let cores: std::collections::BTreeMap<String, String> =
                req["core_dirs"].as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|d| (k.clone(), d.to_string()))).collect()).unwrap_or_default();
            crate::chains::plan(&node.self_arc(), &keys, &s("mode").unwrap_or_else(|| "full".into()), req["skip"] == true, &cores)
        }
        // By hand: a resume entry in the queue for the chain is dropped (chains::pause_by_hand).
        "chains.pause" => crate::chains::pause_by_hand(node, &s("key").ok_or("key required")?, true),
        "chains.resume" => crate::chains::pause_by_hand(node, &s("key").ok_or("key required")?, false),
        "chains.index_add" => crate::chains::add_index(&node.self_arc(), &s("key").ok_or("key required")?, &s("index").ok_or("index required")?),
        "chains.index_remove" => crate::chains::remove_index(&node.self_arc(), &s("key").ok_or("key required")?, &s("index").ok_or("index required")?),
        "chains.remove" => crate::chains::remove(&node.self_arc(), &s("key").ok_or("key required")?),
        // Core-data copies (crate::coreboot)
        "chains.copy_status" => Ok(crate::coreboot::status(node, s("key").as_deref())),
        "chains.copy_start" => crate::coreboot::start(&node.self_arc(), &s("key").ok_or("key required")?, &s("what").unwrap_or_else(|| "coins".into())),
        "chains.copy_from_dir" => crate::coreboot::start_from_dir(&node.self_arc(), &s("key").ok_or("key required")?, &s("dir").ok_or("dir required (a Core datadir on this computer)")?),
        "chains.copy_cancel" => crate::coreboot::cancel(node, &s("key").ok_or("key required")?),
        "chains.refill" => crate::refill::start(node.self_arc(), &s("key").ok_or("key required")?, req["from"].as_u64(), req["to"].as_u64(), req["sources"].as_u64()),
        "chains.refill_status" => Ok(crate::refill::status(s("key").as_deref())),
        "chains.refill_cancel" => crate::refill::cancel(&s("key").ok_or("key required")?),
        "chains.check_rules" => crate::rulecheck::start(node.self_arc(), &s("key").ok_or("key required")?, req["from"].as_u64(), req["to"].as_u64(), req["threads"].as_u64(), req["scripts"] == "assume"),
        "chains.check_rules_status" => Ok(crate::rulecheck::status(s("key").as_deref())),
        "chains.check_rules_cancel" => crate::rulecheck::cancel(&s("key").ok_or("key required")?),
        "update.status" => Ok(crate::updater::status(node)),
        "update.set" => crate::updater::set(node, req["on"] == true),
        "update.check" => crate::updater::check().map(|_| crate::updater::status(node)),
        "update.now" => crate::updater::update_now(node.self_arc()),
        "chains.rule_reports" => Ok(crate::rulecheck::reports(node, s("key").as_deref())),
        "chains.copy_verify" => {
            let key = s("key").ok_or("key required")?;
            let p = arcade_chain::params::by_key(&key).ok_or("unknown chain")?;
            let started = crate::cbimport::start_verify(node.self_arc(), p.key.to_string());
            Ok(serde_json::json!({"started": started, "note": crate::cbimport::verify_note(p.key)}))
        }
        // Index copies from old arcades (crate::lxfetch)
        #[cfg(feature = "oldimport")]
        "chains.index_copy_status" => {
            let k = s("key");
            Ok(json!({"ledger": crate::lxfetch::status(node, k.as_deref()), "ord": crate::dxfetch::status(node, k.as_deref())}))
        }
        #[cfg(feature = "oldimport")]
        "chains.index_copy_start" | "chains.index_copy_cancel" => {
            let key = s("key").ok_or("key required")?;
            let start = req["cmd"] == "chains.index_copy_start";
            match s("index").as_deref() {
                Some("ledger") if start => crate::lxfetch::start(node, &key, s("from")),
                Some("ledger") => crate::lxfetch::cancel(node, &key),
                Some("ord") if start => crate::dxfetch::start(node, &key, s("from")),
                Some("ord") => crate::dxfetch::cancel(node, &key),
                _ => Err("index = ledger or ord".into()),
            }
        }
        // V1.0 copies (crate::v1copy)
        "share.held" => Ok(crate::v1copy::held(node)),
        "share.offers" => crate::vxshare::offers_json(node, &s("key").ok_or("key required")?),
        "share.fetch" => crate::vxshare::start(node, &s("key").ok_or("key required")?, &s("fam").ok_or("fam required")?, s("id")),
        "share.install" => crate::v1import::start(node.self_arc(), &s("key").ok_or("key required")?),
        "share.install_cancel" => crate::v1import::cancel(&s("key").ok_or("key required")?),
        "share.cancel" => crate::vxshare::cancel(node, &s("key").ok_or("key required")?, &s("fam").ok_or("fam required")?),
        "share.serve" => crate::v1copy::set_serve(node, &s("key").ok_or("key required")?, req["on"] != false),
        "share.make" => crate::v1copy::start_make(node.self_arc(), &s("key").ok_or("key required")?, &s("fam").ok_or("fam required (blocks, coins, ord, ...)")?),
        // Borrowed coins (crate::borrow)
        "borrow.status" => Ok(crate::borrow::status_all(node)),
        "borrow.set" => {
            let on = req["on"].as_bool().ok_or("on required (true or false)")?;
            crate::borrow::set(node, s("chain").as_deref(), on)
        }
        "borrow.coins" => crate::borrow::coins(node, &s("chain").ok_or("chain required")?),
        "borrow.send" => {
            let amount = req["amount"].as_u64().ok_or("amount required (satoshis)")?;
            crate::borrow::send(node, &s("chain").ok_or("chain required")?, &s("to").ok_or("to required")?, amount, s("from").as_deref(), s("kind").as_deref(), req["all_kinds"] == true, req["send"] == true)
        }
        "events" => {
            let n = req["n"].as_u64().unwrap_or(30).min(2000) as usize;
            let kind = s("kind");
            let ev = node.app.recent_events(if kind.is_some() { 2000 } else { n }).map_err(|e| e.to_string())?;
            Ok(json!(ev.into_iter().filter(|e| kind.as_deref().is_none_or(|k| e.kind == k)).take(n).collect::<Vec<_>>()))
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

#[cfg(test)]
mod start_tests {
    use super::*;

    #[test]
    fn requests_wait_for_the_start_then_are_answered() {
        // Before set_ready a request is told "starting" (the window shows "starting the service"); after it, answered.
        let r = starting_reply();
        assert_eq!(r["ok"], false);
        assert_eq!(r["starting"], true);
        let t = std::thread::spawn(|| wait_ready(std::time::Duration::from_secs(20)));
        std::thread::sleep(std::time::Duration::from_millis(50));
        set_ready();
        assert!(t.join().unwrap(), "a waiting request goes on once arcaded has started");
        assert!(wait_ready(std::time::Duration::ZERO));
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;

    #[test]
    fn errors_carry_a_stable_code() {
        assert_eq!(error_code("unknown command \"x.y\""), "unknown_command");
        assert_eq!(error_code("unknown route /api/v1/r/mesh: GET /api/v1/routes lists them"), "unknown_route");
        assert_eq!(error_code("not enough ARCADE: 0.1 needed, 0.05 here"), "not_enough_arcade");
        assert_eq!(error_code("not enough coins for 5 DOGE"), "insufficient_funds");
        assert_eq!(error_code("no transaction 55dd on ARCADE testnet (asked the mesh too: x)"), "not_found");
        assert_eq!(error_code("repo required"), "bad_args");
        assert_eq!(error_code("state is open, closed or merged, not bogus"), "bad_args");
        assert_eq!(error_code("until: accepted_local, broadcast_ok, seen_in_peer_mempool or confirmed, not x"), "bad_args");
        assert_eq!(error_code("ARCADE testnet is not installed"), "not_installed");
        assert_eq!(error_code("unknown chain zzz"), "unknown_chain");
        assert_eq!(error_code("something else"), "error");
        assert_eq!(error_reply("repo required")["code"], "bad_args");
        assert_eq!(stopping_reply()["state"], "stopping");
        assert_eq!(starting_reply()["state"], "starting");
    }

    #[test]
    fn items_are_filtered_by_state_kind_and_height() {
        let v = json!({"repo": "@a/b", "items": [
            {"number": 1, "kind": "issue", "state": "open", "height": 10, "text": {"title": "one", "body": "x"}, "events": [{"w": "comment", "height": 30}]},
            {"number": 2, "kind": "pull", "state": "merged", "height": 20, "text": {"title": "two"}, "events": []},
            {"number": 3, "kind": "issue", "state": "closed", "height": 5, "text": {"title": "three"}, "events": []},
        ]});
        let n = |r: Value| r["items"].as_array().unwrap().iter().map(|i| i["number"].as_u64().unwrap()).collect::<Vec<_>>();
        assert_eq!(n(filter_items(v.clone(), &json!({"state": "open"})).unwrap()), vec![1]);
        assert_eq!(n(filter_items(v.clone(), &json!({"kind": "issue"})).unwrap()), vec![1, 3]);
        assert_eq!(n(filter_items(v.clone(), &json!({"since": 15})).unwrap()), vec![1, 2], "a comment at 30 counts");
        let s = filter_items(v.clone(), &json!({"summary": true})).unwrap();
        assert!(s["items"][0].get("events").is_none() && s["items"][0]["text"] == json!({"title": "one"}) && s["items"][0]["comments"] == 1);
        assert_eq!(s["count"], 3);
        assert!(filter_items(v.clone(), &json!({"state": "shut"})).is_err());
    }

    /// 2026-11-21 (ARCADE #13): since trims each item's events, every item says its last_height, and a summary also
    /// says last_by. An item with 11 comments read with since keeps only the newer ones.
    #[test]
    fn since_trims_events_and_items_say_their_last_activity() {
        let body = "x".repeat(1800);
        let mut events: Vec<Value> = (0..11u64).map(|i| json!({"w": "comment", "by": format!("@user{i}"), "height": 3000 + i * 70, "text": {"body": body}})).collect();
        events.push(json!({"w": "comment", "by": "@waiting", "height": null, "pending": true, "text": {"body": "soon"}}));
        let v = json!({"repo": "@a/b", "items": [
            {"number": 13, "kind": "issue", "by": "@opener", "state": "open", "height": 2900, "text": {"title": "t", "body": body}, "events": events},
            {"number": 14, "kind": "issue", "by": "@quiet", "state": "open", "height": 2950, "text": {"title": "q"}, "events": []},
        ]});
        // No since: every event stays, and last_height is the newest confirmed event's.
        let all = filter_items(v.clone(), &json!({})).unwrap();
        assert_eq!(all["items"][0]["events"].as_array().unwrap().len(), 12);
        assert_eq!(all["items"][0]["last_height"], 3700);
        assert_eq!(all["items"][1]["last_height"], 2950, "no events: the item's own height");
        assert!(all["items"][0].get("last_by").is_none(), "last_by only in summaries");
        // since 3700: only the comment at 3700 and the pending one are left; comments and last_height are unchanged.
        let t = filter_items(v.clone(), &json!({"since": 3700})).unwrap();
        let it = &t["items"][0];
        assert_eq!(t["count"], 1, "items are chosen as before");
        let hs: Vec<Value> = it["events"].as_array().unwrap().iter().map(|e| e["height"].clone()).collect();
        assert_eq!(hs, vec![json!(3700), Value::Null]);
        assert_eq!(it["comments"], 12);
        assert_eq!(it["last_height"], 3700);
        let (a, b) = (serde_json::to_vec(&all["items"][0]).unwrap().len(), serde_json::to_vec(it).unwrap().len());
        println!("item 13: {a} bytes without since, {b} with since 3700");
        assert!(b * 4 < a);
        // A summary names who made the newest event.
        let s = filter_items(v, &json!({"summary": true})).unwrap();
        assert_eq!(s["items"][0]["last_height"], 3700);
        assert_eq!(s["items"][0]["last_by"], "@user10");
        assert_eq!(s["items"][1]["last_by"], "@quiet", "no events: the item's author");
        assert!(s["items"][0].get("events").is_none());
    }

    #[test]
    fn items_page_with_a_cursor_and_pending_ones_come_last() {
        let it = |n: u64| json!({"number": n, "kind": "issue", "state": "open", "height": n, "text": {}, "events": []});
        let mut items: Vec<Value> = (1..=5).rev().map(it).collect();
        items.push(json!({"number": null, "kind": "issue", "state": "open", "height": null, "txid": "ab", "text": {}, "events": [], "pending": true}));
        let v = json!({"repo": "@a/b", "items": items});
        let nums = |r: &Value| r["items"].as_array().unwrap().iter().map(|i| i["number"].as_u64()).collect::<Vec<_>>();
        let p1 = page_items(filter_items(v.clone(), &json!({})).unwrap(), &json!({"limit": 2})).unwrap();
        assert_eq!(nums(&p1), vec![Some(1), Some(2)]);
        assert_eq!(p1["next"], "n.2");
        let p2 = page_items(v.clone(), &json!({"limit": 2, "cursor": "n.2"})).unwrap();
        assert_eq!(nums(&p2), vec![Some(3), Some(4)]);
        let p3 = page_items(v.clone(), &json!({"limit": 2, "cursor": p2["next"]})).unwrap();
        assert_eq!(nums(&p3), vec![Some(5), None], "the last page carries the pending item");
        assert!(p3["next"].is_null());
        assert!(page_items(v.clone(), &json!({"cursor": "bogus"})).is_err());
        // A since filter keeps what is pending (it is newer than any height).
        assert_eq!(filter_items(v, &json!({"since": 5})).unwrap()["count"], 2);
    }

    #[test]
    fn events_since_follows_the_log_with_a_cursor() {
        let (_g, t) = crate::http::tests::node();
        t.node.app.event("test", "before").unwrap();
        let now = events_since(&t.node, &json!({})).unwrap();
        assert!(now["events"].as_array().unwrap().is_empty(), "no cursor: from now");
        t.node.app.event("test", "one").unwrap();
        t.node.app.event("other", "two").unwrap();
        let a = events_since(&t.node, &json!({"cursor": now["next"]})).unwrap();
        let texts = |v: &Value| v["events"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(texts(&a), vec!["one", "two"]);
        let b = events_since(&t.node, &json!({"cursor": now["next"], "kinds": "other"})).unwrap();
        assert_eq!(texts(&b), vec!["two"]);
        let c = events_since(&t.node, &json!({"cursor": a["next"], "wait": 1})).unwrap();
        assert!(c["events"].as_array().unwrap().is_empty() && c["timed_out"] == true);
        let all = events_since(&t.node, &json!({"cursor": "0"})).unwrap();
        assert!(texts(&all).contains(&"before".to_string()));
        assert!(events_since(&t.node, &json!({"cursor": "zz"})).is_err());
    }

    #[test]
    fn a_name_outside_the_table_is_no_command_on_any_surface() {
        let (_g, t) = crate::http::tests::node();
        let e = dispatch(&t.node, &json!({"cmd": "no.such_thing"})).unwrap_err();
        assert_eq!(error_code(&e), "unknown_command");
        let e = dispatch(&t.node, &json!({"cmd": "service.stop"})).unwrap_err();
        assert_eq!(error_code(&e), "unknown_command", "service.stop is the socket's own line, never dispatched");
        assert!(dispatch(&t.node, &json!({"cmd": "rpc.openapi"})).unwrap()["paths"].is_object());
        assert!(dispatch(&t.node, &json!({"cmd": "rpc.guide"})).unwrap().as_str().unwrap().contains("client_ref"));
        // 2026-11-21 (ARCADE #13): the filters reach the socket.
        let h = dispatch(&t.node, &json!({"cmd": "rpc.help", "method": "tx.wait"})).unwrap();
        assert_eq!(h.as_array().unwrap().len(), 1);
        let o = dispatch(&t.node, &json!({"cmd": "rpc.openapi", "prefix": "tx."})).unwrap();
        assert!(o["paths"].as_object().unwrap().keys().all(|k| k.starts_with("/api/v1/tx/")));
        assert_eq!(dispatch(&t.node, &json!({"cmd": "rpc.help", "prefix": ""})).unwrap(), crate::commands::help(), "empty is no filter");
        for c in ERROR_CODES {
            assert!(!c.is_empty());
        }
        let w = dispatch(&t.node, &json!({"cmd": "chains.wait", "chain": t.node.params.key, "height": 0, "timeout": 1})).unwrap();
        assert_eq!(w["chain"], t.node.params.key);
        assert!(w["source"].is_string());
        let late = dispatch(&t.node, &json!({"cmd": "chains.wait", "chain": t.node.params.key, "height": 1_000_000_000u64, "timeout": 1})).unwrap();
        assert!(late["timed_out"] == true && late["reached"] == false);
        assert_eq!(error_code(&dispatch(&t.node, &json!({"cmd": "chains.wait", "chain": "nochain", "height": 1})).unwrap_err()), "unknown_chain");
        let feed = dispatch(&t.node, &json!({"cmd": "explorer.blocks", "order": "asc", "from": 0, "limit": 5})).unwrap();
        assert_eq!(feed["order"], "asc");
        assert!(feed["next_from"].is_u64() && feed["source"].is_string());
        assert_eq!(error_code(&dispatch(&t.node, &json!({"cmd": "explorer.blocks", "order": "sideways"})).unwrap_err()), "bad_args");
    }
}
