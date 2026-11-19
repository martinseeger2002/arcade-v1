//! Local API: one JSON request per line on a Unix socket, one JSON answer per line.
//! Used by the `arcade` CLI and the desktop app.

use crate::node::Node;
use arcade_local::{UnixListener, UnixStream};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;

pub fn serve(node: Arc<Node>, path: std::path::PathBuf) {
    if let Err(e) = arcade_local::check_socket_path(&path) {
        panic!("bind API socket: {e}");
    }
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path).unwrap_or_else(|e| panic!("bind API socket {}: {e}", path.display()));
    // Group-readable on Linux (the service user and the arcade group). On Windows the
    // socket lives in the user's own local app data, which other users cannot read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660));
    }
    for s in l.incoming().flatten() {
        let node = node.clone();
        std::thread::spawn(move || handle(node, s));
    }
}

fn handle(node: Arc<Node>, s: UnixStream) {
    let mut w = s.try_clone().unwrap();
    let mut lines = BufReader::new(s).lines();
    while let Some(Ok(line)) = lines.next() {
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
                    Err(e) => json!({"ok": false, "error": e}),
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
                Err(e) => json!({"ok": false, "error": e}),
            },
            Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
        };
        if writeln!(w, "{reply}").is_err() {
            break;
        }
    }
}

/// Make this arcade's wallet: new words, or restored from `phrase` (with its @handle, when the
/// chain already shows it). New and restored wallets alike ask the mesh for a test coin.
pub fn create_wallet(node: &Node, phrase: Option<&str>) -> Result<Value, String> {
    let p = node.params;
    let restored = phrase.is_some();
    let words = node.app.create_wallet(phrase).map_err(|e| e.to_string())?;
    // A restored wallet's @handle, found on chain right away (else after a later sync round).
    let tag = if restored { crate::claims::find_own_tag(node, true) } else { None };
    // Test coins for the first handle come by themselves: asked for now, so they are
    // usually here by the time a handle is picked (a restored wallet may have some).
    // On ARCADE as the home chain (crate::claims::claim_home) the node faucet's ARCADE pays the claim instead.
    if tag.is_none() && !crate::arcsvc::runs(crate::claims::claim_home(node)) {
        if let Ok((k, _)) = node.app.tag_key(p, 0) {
            let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&k.pubkey).unwrap()).unwrap());
            if crate::claims::coins(node, &script).is_empty() {
                let _ = crate::faucet::ask_if_needed(node, &k.address, "");
            }
        }
    }
    // And this arcade's 1 ARCADE from the node faucet (crate::arcfaucet), asked now rather than at the next round.
    crate::arcfaucet::node_gift_ask(node);
    Ok(json!({"phrase": if restored { Value::Null } else { json!(words) }, "restored": restored, "tag": tag}))
}

pub fn dispatch(node: &Node, req: &Value) -> Result<Value, String> {
    // `"as": "@tag"` on an ARCADE write (ledger.*, a.*, arc.blob_publish*): signed by the key holding that tag, the
    // wallet's own or an imported one (crate::arcledger::with_sign_as).
    let cmd = req["cmd"].as_str().unwrap_or("");
    if let Some(tag) = req["as"].as_str().filter(|_| cmd.starts_with("ledger.") || cmd.starts_with("a.") || cmd.starts_with("arc.blob_publish")) {
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
    let s = |k: &str| req[k].as_str().map(String::from);
    let p = node.params;
    let c = p.chain_no;
    match req["cmd"].as_str().unwrap_or("") {
        // The link that opens the desktop app's window in a browser (crate::webui; `arcade web`).
        "ui.link" => crate::webui::link(node, node.http_port.get().copied().unwrap_or(8420)),
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
            Ok(json!({
                "chain": p.name,
                "sync": *node.sync_state.lock().unwrap(),
                "tip": tip.map(|t| json!({"height": t.height, "hash": t.hash.to_hex()})),
                "best_known": best,
                "tags": {"indexed_to": indexed_to, "count": tag_count, "names_digest": digest},
                "profiles": index::profile_count(&node.chain, c).unwrap_or(0),
                "mesh": {"links": node.links.lock().unwrap().len(), "tips": mesh},
                "node_id": node.id_hex(),
                "wallet": node.app.has_wallet().unwrap_or(false),
            }))
        }
        "mesh.udp" => Ok(crate::udpsvc::status(node)),
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
            let r = crate::coinsend::send(node, s("chain").as_deref(), &s("to").ok_or("to required")?, amount, s("from").as_deref(), s("kind").as_deref(), req["all_kinds"] == true, req["send"] == true);
            crate::coinsend::OWNER_CHANGE_OK.with(|c| c.set(false));
            r
        }
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
        "msg.stats" => Ok(arcade_ledger::messages::counts(&node.chain, node.ledger_home().chain_no)),
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
                Some(t) => Some(arcade_ledger::index::tag_row(&node.chain, p.chain_no, &t.trim_start_matches('@').to_lowercase()).map_err(|e| e.to_string())?.ok_or_else(|| format!("@{t} is not claimed"))?.address),
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
        "apps.store" => Ok(crate::appsvc::store(node, &s("sort").unwrap_or_else(|| "popular".into()), &s("q").unwrap_or_default(), req["flagged"] == true, &s("category").unwrap_or_default())),
        "apps.categories" => Ok(crate::appsvc::categories()),
        "apps.installed" => Ok(crate::appsvc::installed(node)),
        "apps.install" => crate::appsvc::install(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?),
        "apps.uninstall" => crate::appsvc::uninstall(node, &s("id").ok_or("id required")?),
        "apps.star" => crate::appsvc::star(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?, req["on"] != false),
        "apps.card" => crate::appsvc::card(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?),
        "apps.perms" => {
            let id = s("id").ok_or("id required")?;
            if req["set"].is_object() { crate::appsvc::set_perms(node, &id, &req["set"]) } else { Ok(crate::appsvc::perms(node, &id)) }
        }
        "apps.review" => crate::appsvc::review(node, &s("chain").ok_or("chain required")?, &s("txid").ok_or("txid required")?, &s("verdict").ok_or("verdict required (clear or confirm)")?),
        "apps.trust" => crate::appsvc::trust(node, &s("address").ok_or("address required")?, req["on"] != false),
        "inscribe" | "inscribe.estimate" => {
            use base64::Engine;
            let data = req["data"].as_str().unwrap_or("");
            let bytes = if req["encoding"] == "base64" { base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("data: {e}"))? } else { data.as_bytes().to_vec() };
            let ctype = s("contenttype").unwrap_or_else(|| "text/plain".into());
            let json = match &req["json"] { Value::Null => None, Value::String(j) => Some(j.clone()), other => Some(other.to_string()) };
            if req["cmd"] == "inscribe.estimate" {
                Ok(crate::walletsvc::inscribe_estimate(bytes.len(), &ctype, json.as_ref().map(|j| j.len()).unwrap_or(0)))
            } else {
                crate::walletsvc::inscribe(node, &bytes, &ctype, json.as_deref())
            }
        }
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
        "realtime.lanes" => crate::realtime::fast::lanes(node, &s("token").ok_or("token required")?),
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
        "rpc.help" => Ok(crate::commands::help()),
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
        "tag.check" => Ok(json!(crate::claims::check_on(node, crate::claims::chain_for(node, s("chain").as_deref())?, &s("tag").ok_or("tag required")?))),
        "tag.claim" => Ok(json!(crate::claims::start_on(node, crate::claims::chain_for(node, s("chain").as_deref())?, &s("tag").ok_or("tag required")?)?)),
        // The ledger on the ARCADE chain (§15 step 7).
        "ledger.announce" => crate::arcledger::announce(node),
        "ledger.profile" => crate::arcledger::profile(node, &req),
        "ledger.repo_push" => crate::arcledger::repo_push(node, &s("name").ok_or("name required")?, &s("manifest").ok_or("manifest required")?, s("base").as_deref(), req["force"] == true),
        "ledger.repo_maintainer" => crate::arcledger::repo_maintainer(node, &s("name").ok_or("name required")?, &s("tag").ok_or("tag required")?, req["add"] != false),
        "ledger.repo_fork" => crate::arcledger::repo_fork(node, &s("from").ok_or("from required")?, &s("name").ok_or("name required")?),
        "ledger.item_open" => crate::arcledger::item_open(node, &req),
        "ledger.item_comment" => crate::arcledger::item_comment(node, &req),
        "ledger.item_state" => crate::arcledger::item_state(node, &req),
        "ledger.items" => crate::arcledger::items(node, &req),
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
        "a.post" => crate::arcledger::a_post(node, &s("text").ok_or("text required")?),
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
        "mempool" => {
            // The chain asked for (`--chain`), the home chain by default; the ARCADE chain's pool is its own.
            let p = crate::sitesvc::chain(node, req)?;
            let pool = if crate::arcsvc::runs(p) {
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
            let threads = s("threads").and_then(|t| t.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1));
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
            let pc = crate::sitesvc::chain(node, req)?;
            let ledgers: Vec<u16> = if pc.chain_no == c { vec![c] } else { vec![pc.chain_no, c] };
            let q = s("who").ok_or("give an @tag or an address")?;
            let address = match q.strip_prefix('@') {
                Some(t) => {
                    let t = t.to_lowercase();
                    let (l, row) = ledgers.iter().find_map(|&l| Some((l, index::tag_row(&node.chain, l, &t).ok().flatten()?)))
                        .ok_or_else(|| format!("@{t} is not claimed on {}", pc.name))?;
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
            let tag = ledgers.iter().find_map(|&l| index::tag_of(&node.chain, l, &address).ok().flatten());
            let mut prof = ledgers.iter().find_map(|&l| index::profile(&node.chain, l, &address).ok().flatten());
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
                Some(sc) => (node.chain.balance_for_script(pc.chain_no, sc).unwrap_or(0), node.chain.history_for_script(pc.chain_no, sc).map(|h| h.len()).unwrap_or(0)),
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
            Ok(json!({"chain": pc.key, "address": address, "tag": tag, "balance": balance, "history_rows": history,
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
            match req["cmd"].as_str().unwrap() {
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
            }
        }
        // This arcade's ARCADE pay and Earn progress, read only (arc.earn without the switch).
        "explorer.earn" => crate::arcsvc::earn_read(node, &req),
        c if c.starts_with("explorer.") => crate::explorer::handle_with(node, c, &req, req["img"] == "links"),
        c @ ("ledger.book" | "ledger.trades" | "ledger.pairs") => crate::marketsvc::handle(node, c, &req),
        // Token sends from this wallet (tokensend.rs): arcade tokens in one transaction; PRC-20 / DRC-20 as a job.
        "token.send" => crate::tokensend::ledger_send(node, req["propertyid"].as_u64().or_else(|| s("propertyid").and_then(|x| x.parse().ok())).ok_or("propertyid required")? as u32,
            req["units"].as_u64().or_else(|| s("units").and_then(|x| x.parse().ok())).ok_or("units required (base units)")?, &s("to").ok_or("to required")?, req["send"] == true),
        "token20.send" => crate::tokensend::t20_send(node, &s("chain").ok_or("chain required")?, &s("tick").ok_or("tick required")?, &s("amount").ok_or("amount required")?, &s("to").ok_or("to required")?, req["send"] == true),
        "token20.sends" => Ok(crate::tokensend::t20_sends(node)),
        "ord.send" => crate::ordsend::send(node, s("chain").as_deref(), &s("inscription").ok_or("inscription required")?, &s("to").ok_or("to required")?, req["send"] == true),
        "swap.place" => crate::swaps::place(node, &req["give"], &req["get"], &req),
        "swap.release" => crate::swaps::release(node, &s("order").ok_or("order required")?),
        "swap.book" => Ok(json!(crate::swaps::book(node))),
        // With the legs shown ({give, get}), only on exactly those terms.
        "swap.take" => crate::swaps::take(node, &s("node").ok_or("node required")?, &s("order").ok_or("order required")?, req["give"].is_object().then_some(req)),
        "swap.list" => Ok(json!(crate::swaps::swaps(node))),
        c @ ("swap.quote" | "swap.told" | "swap.status") => crate::swaps::outside(node, &c[5..], &req),
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
            Ok(st)
        }
        "chains.drives" => Ok(crate::chaindrive::drives(node)),
        "chains.move" => crate::chaindrive::start(&node.self_arc(), &s("key").ok_or("key required")?, &s("drive").unwrap_or_else(|| "home".into())),
        "arc.status" => crate::arcsvc::status(node),
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
                let path = f["path"].as_str().ok_or("each file needs its path")?;
                let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
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
        "arc.blob_publish" => {
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
            // The drive chosen for it, else this arcade's own (crate::chaindrive): its file starts there.
            crate::chaindrive::add(node, &key, &s("drive").unwrap_or_default(), || crate::chains::add(&node.self_arc(), &key, &s("mode").unwrap_or_else(|| "full".into()), with))
        }
        "chains.queue" => Ok(crate::chains::queue_list(node)),
        "chains.unqueue" => crate::chains::unqueue(node, &s("key").ok_or("key required")?),
        "chains.pause" => crate::chains::set_paused(node, &s("key").ok_or("key required")?, true),
        "chains.resume" => crate::chains::set_paused(node, &s("key").ok_or("key required")?, false),
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
