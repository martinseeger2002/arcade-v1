//! Local API: one JSON request per line on a Unix socket, one JSON answer per line.
//! Used by the `arcade` CLI and the desktop app.

use crate::node::Node;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

pub fn serve(node: Arc<Node>, path: std::path::PathBuf) {
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path).expect("bind API socket");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660) /* service user + arcade group */);
    for s in l.incoming().flatten() {
        let node = node.clone();
        std::thread::spawn(move || handle(node, s));
    }
}

fn handle(node: Arc<Node>, s: UnixStream) {
    let mut w = s.try_clone().unwrap();
    for line in BufReader::new(s).lines().map_while(Result::ok) {
        let reply = match serde_json::from_str::<Value>(&line) {
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

pub fn dispatch(node: &Node, req: &Value) -> Result<Value, String> {
    let s = |k: &str| req[k].as_str().map(String::from);
    let p = node.params;
    let c = p.chain_no;
    match req["cmd"].as_str().unwrap_or("") {
        // The link that opens the desktop app's window in a browser (crate::webui; `arcade web`).
        "ui.link" => crate::webui::link(node, node.http_port.get().copied().unwrap_or(8420)),
        "status" => {
            let tip = node.chain.tip(c).map_err(|e| e.to_string())?;
            use arcade_ledger::index;
            let tag_count = index::read_tags(&node.chain, c).map(|t| t.len()).unwrap_or(0);
            let digest = index::names_digest(&node.chain, c).unwrap_or_default();
            let indexed_to = node.chain.indexed_to(c, "ledger.v1").ok().flatten();
            let tips = node.mesh_tips.lock().unwrap();
            let mesh: Vec<Value> = tips
                .iter()
                .map(|(a, t)| json!({"peer": a, "height": t.height, "names_digest": t.names_digest}))
                .collect();
            Ok(json!({
                "chain": p.name,
                "sync": *node.sync_state.lock().unwrap(),
                "tip": tip.map(|t| json!({"height": t.height, "hash": t.hash.to_hex()})),
                "tags": {"indexed_to": indexed_to, "count": tag_count, "names_digest": digest},
                "profiles": index::profile_count(&node.chain, c).unwrap_or(0),
                "mesh": {"links": node.links.lock().unwrap().len(), "tips": mesh},
                "node_id": node.id_hex(),
                "wallet": node.app.has_wallet().unwrap_or(false),
            }))
        }
        "mesh.udp" => Ok(crate::udpsvc::status(node)),
        "mesh.udp_set" => {
            if let Some(p) = req["drop"].as_str().filter(|p| !p.is_empty()) {
                return Ok(json!({"dropped": crate::udpsvc::drop_path(p)}));
            }
            crate::udpsvc::set(node, req)
        }
        "peers" => Ok(json!(node.links.lock().unwrap().values().map(|l| json!({"addr": l.addr, "node_id": l.node_id, "since": l.since})).collect::<Vec<_>>())),
        "wallet.create" => {
            let phrase = node.app.create_wallet(s("phrase").as_deref()).map_err(|e| e.to_string())?;
            // Test coins for the first handle come by themselves: asked for now, so they are
            // usually here by the time a handle is picked (a restored wallet may have some).
            if let Ok((k, _)) = node.app.tag_key(p, 0) {
                let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&k.pubkey).unwrap()).unwrap());
                if crate::claims::coins(node, &script).is_empty() {
                    let _ = crate::faucet::ask_if_needed(node, &k.address, "");
                }
            }
            let restored = s("phrase").is_some();
            Ok(json!({"phrase": if restored { Value::Null } else { json!(phrase) }, "restored": restored}))
        }
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
            Ok(json!(keys.iter().map(|k| {
                let pk = arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&k.pubkey).unwrap()).unwrap();
                // Every address of the key (DigiByte: dgb1... to receive, its D... still counted).
                let scripts = crate::coinsend::key_scripts(cp, &pk);
                let bal = |sc: &[u8]| if installed { node.chain.balance_for_script(cp.chain_no, sc).ok() } else { crate::borrow::balance(node, cp, sc) };
                let balance = scripts.iter().map(|(_, sc)| bal(sc)).try_fold(0u64, |a, b| b.map(|x| a + x));
                // Home chain: coins still waiting in the mempool, spendable already.
                let pending: u64 = if cp.chain_no == p.chain_no && !crate::borrow::is_borrowed(cp.key) { scripts.iter().flat_map(|(_, sc)| crate::claims::unconfirmed_coins(node, sc)).map(|u| u.value).sum() } else { 0 };
                json!({"index": k.index, "address": scripts[0].0, "legacy": scripts.get(1).map(|x| x.0.clone()), "pending": pending,
                       "tag": if cp.chain_no == p.chain_no { k.tag.clone() } else { None },
                       "balance": balance, "borrowed": !installed && crate::borrow::is_borrowed(cp.key)})
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
            crate::coinsend::send(node, s("chain").as_deref(), &s("to").ok_or("to required")?, amount, s("from").as_deref(), req["send"] == true)
        }
        "wallet.link" => crate::links::link(node, &s("chain").ok_or("chain required")?, &s("wif").ok_or("wif required")?, &s("tag").ok_or("tag required")?),
        "wallet.unlink" => crate::links::unlink(node, &s("chain").ok_or("chain required")?, &s("address").ok_or("address required")?, &s("tag").ok_or("tag required")?),
        "account.signin" => crate::accountsvc::sign_in(node, &s("tag").ok_or("tag required")?, &s("password").ok_or("password required")?, s("from").as_deref().filter(|f| !f.is_empty())),
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
        "msg.stats" => Ok(arcade_ledger::messages::counts(&node.chain, node.params.chain_no)),
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
        "contacts.list" => Ok(crate::msgsvc::contacts_list(node)),
        "contacts.save" => crate::msgsvc::contact_save(node, &s("key").ok_or("key required")?, &s("name").unwrap_or_default(), &s("address").unwrap_or_default(), &s("notes").unwrap_or_default()),
        "contacts.delete" => crate::msgsvc::contact_delete(node, &s("key").ok_or("key required")?),
        "mute.list" => Ok(json!(crate::msgsvc::mutes(node))),
        "mute.add" => crate::msgsvc::mute(node, &s("address").ok_or("address required")?, true),
        "mute.remove" => crate::msgsvc::mute(node, &s("address").ok_or("address required")?, false),
        "tag.check" => Ok(json!(crate::claims::check(node, &s("tag").ok_or("tag required")?))),
        "tag.claim" => Ok(json!(crate::claims::start(node, &s("tag").ok_or("tag required")?)?)),
        "tag.list" => Ok(json!(node.app.claims().map_err(|e| e.to_string())?)),
        "start.status" => Ok(crate::claims::start_status(node)),
        "faucet.ask" => {
            let (k, _) = node.app.tag_key(p, s("index").and_then(|i| i.parse().ok()).unwrap_or(0)).map_err(|e| e.to_string())?;
            Ok(json!(crate::faucet::ask(node, &k.address, &s("tag").unwrap_or_default())?))
        }
        "mempool" => {
            let pool = node.chain.mempool(c).map_err(|e| e.to_string())?;
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
            let q = s("who").ok_or("give an @tag or an address")?;
            let address = match q.strip_prefix('@') {
                Some(t) => index::tag_row(&node.chain, c, &t.to_lowercase()).map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("@{t} is not claimed on {}", p.name))?.address,
                None => q.clone(),
            };
            let tag = index::tag_of(&node.chain, c, &address).map_err(|e| e.to_string())?;
            let mut prof = index::profile(&node.chain, c, &address).map_err(|e| e.to_string())?;
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
            let script = arcade_ledger::address::script_for_address(p, &address);
            let (balance, history) = match &script {
                Some(sc) => (node.chain.balance_for_script(c, sc).unwrap_or(0), node.chain.history_for_script(c, sc).map(|h| h.len()).unwrap_or(0)),
                None => (0, 0),
            };
            // Does someone else's profile state this address as theirs?
            let linked_by = index::owner_of_linked(&node.chain, c, p.key, &address).unwrap_or(None);
            // The tag in a profile is only a claim; verified when the tag table agrees.
            let tag_verified = match (prof.as_ref().and_then(|v| v["tag"].as_str()), &tag) {
                (Some(t), Some(real)) => Some(t == real),
                (Some(_), None) => Some(false),
                _ => None,
            };
            Ok(json!({"address": address, "tag": tag, "balance": balance, "history_rows": history,
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
        "chains.list" => Ok(crate::chains::list(node)),
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
        "arc.open" => crate::arcsvc::open(node, req),
        "arc.sealing" => crate::arcsvc::sealing(node, req),
        "arc.earn" => crate::arcsvc::earn(node, req),
        "chains.add" => {
            let with: Vec<String> = req["with"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            // A drive chosen for it (crate::chaindrive): its file starts there.
            if let (Some(d), Some(p)) = (s("drive").filter(|d| !d.is_empty() && d != "home"), s("key").as_deref().and_then(crate::chains::find)) {
                if !node.extra.lock().unwrap().contains_key(p.key) && p.chain_no != node.params.chain_no {
                    crate::chaindrive::place_new(node, p.chain_no, p.key, &d)?;
                }
            }
            crate::chains::add(&node.self_arc(), &s("key").ok_or("key required")?, &s("mode").unwrap_or_else(|| "full".into()), with)
        }
        "chains.unqueue" => crate::chains::unqueue(node, &s("key").ok_or("key required")?),
        "chains.pause" => crate::chains::set_paused(node, &s("key").ok_or("key required")?, true),
        "chains.resume" => crate::chains::set_paused(node, &s("key").ok_or("key required")?, false),
        "chains.index_add" => crate::chains::add_index(&node.self_arc(), &s("key").ok_or("key required")?, &s("index").ok_or("index required")?),
        "chains.index_remove" => crate::chains::remove_index(&node.self_arc(), &s("key").ok_or("key required")?, &s("index").ok_or("index required")?),
        "chains.remove" => crate::chains::remove(&node.self_arc(), &s("key").ok_or("key required")?),
        // Core-data copies (crate::coreboot)
        "chains.copy_status" => Ok(crate::coreboot::status(node, s("key").as_deref())),
        "chains.copy_start" => crate::coreboot::start(&node.self_arc(), &s("key").ok_or("key required")?, &s("what").unwrap_or_else(|| "coins".into())),
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
            crate::borrow::send(node, &s("chain").ok_or("chain required")?, &s("to").ok_or("to required")?, amount, s("from").as_deref(), req["send"] == true)
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
