# Arcade V1.0 API

Everything the desktop app does goes through one set of commands. Programs reach them three ways:

| Way | Who | Address | Auth | What |
|---|---|---|---|---|
| Local socket | the app, the `arcade` CLI | `<home>/arcaded.sock` | file permissions | every command; one JSON request per line: `{"cmd": "msg.me"}` |
| JSON-RPC 2.0 | sidecar apps on this computer | `POST http://127.0.0.1:8420/rpc` | Basic auth with the cookie file `<home>/.cookie` (`__cookie__:<secret>`, new each start) | every command |
| Web API | websites (their server code) and other programs | `http://127.0.0.1:8420/api/v1/<group>/<name>` | none for **public**; otherwise `Authorization: Bearer ak_...` (keys made in the app's API tab) | commands allowed by the key |

**Only this computer.** `/rpc` and `/api/v1` answer only requests from this computer, addressed to
`127.0.0.1`, `localhost` or `[::1]` with the arcade's port (the `Host` header), that didn't come
through a proxy (requests carrying a proxy's headers, such as `X-Forwarded-*`, `Forwarded`, `Via`,
`X-Real-IP`, `CF-Connecting-IP` or `True-Client-IP`, are refused). A name that merely resolves to
127.0.0.1 (a DNS-rebinding page) is refused, because its `Host` header is not one of those. A reverse
proxy or tunnel that adds none of those headers (a plain `proxy_pass`, `ssh -L`, `socat`) would make
its visitors look like this computer: run arcaded with `--behind-proxy` there, and nothing over the
web port counts as this computer (programs then use the socket). Answers carry no CORS headers. The
web port listens on 127.0.0.1. A non-loopback `--http-bind` is refused unless `--i-understand-remote-api`
is also given, and startup then prints a warning. With that flag the operator can serve a website
(`--site DIR`): visitors get the site's pages and inscription content (`/content/<id>`,
`/inscription/<id>`), never the API, which still requires a loopback `Host`. A public website calls
the API from its own server code.

**The other ports stay open, and they are not this API.** Mesh (TCP 8421) carries blocks and signed
records between arcades: welcome records, code releases, ledger items. STUN (UDP 3478) answers binding
requests so a peer can learn its public address, and carries call media when the relay is on. Neither
serves the wallet or this HTTP API.

## JSON-RPC

```sh
curl -s -u "$(cat <home>/.cookie)" -X POST http://127.0.0.1:8420/rpc \
  -d '{"jsonrpc":"2.0","id":1,"method":"msg.send","params":{"to":"@apple","text":"hi"}}'
```

Named parameters only (`params` is an object). Batches work. Errors: `-32700` parse, `-32600` bad
request, `-32601` unknown method, `-32602` bad params, `-32000` the command's own error (message says why).
`rpc.help` lists every method; `{"method": "tx.wait"}` or `{"prefix": "tx."}` lists only those.

## Web API

```sh
curl -s "http://127.0.0.1:8420/api/v1/feed?order=new&limit=5"                 # public
curl -s -H "Authorization: Bearer ak_..." http://127.0.0.1:8420/api/v1/msg/unread
curl -s -H "Authorization: Bearer ak_..." -X POST http://127.0.0.1:8420/api/v1/msg/send \
  -H "Content-Type: application/json" -d '{"to":"@apple","text":"hi"}'
```

`GET` takes parameters from the query string (numbers and `true`/`false` are read as such);
`POST` takes a JSON object (use it for lists and files). Answers are `{"ok": true, "result": ...}` or
`{"ok": false, "error": "..."}` with status 400 (the command refused), 401 (no or bad key),
403 (the key lacks the permission, or not from this computer), 404 (no such command).
CORS allows any origin, so a page opened in a browser on this computer can call public reads.

### Permissions

| needs | meaning |
|---|---|
| public | anyone who can reach the API (this computer) |
| read | private reads: your messages, groups, contacts, wallet addresses, activity |
| message | send and manage private messages, groups, contacts, mutes |
| post | public posts and feed acts (not tips) |
| tip | tips, up to the key's daily limit (in satoshis) |
| wallet | claim @tags, ask the faucet, link wallets, publish your messaging key |
| admin | chains, indexes, mining |
| local | never over the web API (the 12 words, making a wallet, API keys): socket and cookie RPC only |

Files are `{"name": "...", "type": "image/png", "data": "<base64>"}`. Amounts are satoshis.

## Commands


### General

| method | route | needs | params | what |
|---|---|---|---|---|
| `status` | `/api/v1/status` | public | chain? | Pepecoin testnet: sync state, tip, best_known (the best height anyone tells us of), tags, profiles, mesh links, node id, phase (starting, ready, stopping) and rules.update_needed. With chain (a key, e.g. arctest): that chain alone: tip {height, hash}, best_known (from linked peers, null when none told), best_known_from, behind, sync (synced, syncing, unknown, not_kept), mode and source (own or borrowed), and for the ARCADE chain its rules. |
| `peers` | `/api/v1/peers` | read | — | Arcades linked over the mesh right now. |
| `events` | `/api/v1/events` | read | n?, kind? | Recent activity (newest first), optionally of one kind. events.since follows the log with a cursor. |
| `mempool` | `/api/v1/mempool` | public | — | Pepecoin testnet transactions waiting for a block. |
| `feed` | `/api/v1/feed` | public | order?, channel?, tag?, author?, txid?, limit?, offset? | Public posts (popular, new or friends) with replies, counts and tips. |
| `ledger` | `/api/v1/ledger` | public | — | The arcade ledger's fingerprints next to what other arcades report. |
| `profile` | `/api/v1/profile` | public | who | A published profile (who = @tag or address). |
| `ord` | `/api/v1/ord` | public | id?, chain? | An inscription, or ord index counts. |
| `name` | `/api/v1/name` | public | name?, chain? | A name claim, or counts. |
| `map` | `/api/v1/map` | public | map?, chain? | A map claim, or counts. |
| `token` | `/api/v1/token` | public | tick?, chain? | A token, or how many there are. |
| `tokens` | `/api/v1/tokens` | public | owner, chain? | Token balances of an address. |
| `inscribe` | `/api/v1/inscribe` | wallet | data, encoding?, contenttype?, json?, client_ref? | Inscribe content as this wallet's (arcade INSC; data as text or base64 with encoding: base64). |
| `follow` | `/api/v1/follow` | post | who, on?, client_ref? | Follow an account (on: false unfollows): a feed act from this wallet (on ARCADE, or Pepecoin testnet while ARCADE is stalled); the newest between two accounts counts. |

### Mesh

| method | route | needs | params | what |
|---|---|---|---|---|
| `mesh.udp` | `/api/v1/mesh/udp` | read | — | UDP hole punching: STUN server and proved addresses, NAT kind, which links run over UDP paths (RTT, packets), introductions. `relay`: the media relay (allocations, this month's use, its limit, the address its sockets are reached at). |
| `mesh.udp_set` | `/api/v1/mesh/udp_set` | admin | on?, lan_dial?, stun_port?, turn?, turn_limit_gb?, drop? | Turn UDP paths and introductions on or off (ARCADE_MESH_UDP=0 overrides), offer LAN addresses to every peer, set the STUN port for the next start (0 = none); drop = a peer id prefix whose path to fail now (falls back to TCP). `turn` turns the media relay on the STUN port on or off, `turn_limit_gb` is its monthly limit (100 by default); both from the next start. |
| `mesh.overview` | `/api/v1/mesh/overview` | public | — | The arcade mesh at a glance: every arcade linked or heard lately, its operator, and per chain what it lends, the chain copies it can make (and why not), the index copies it offers, how far it walks ord, and whether its ledger fingerprint matches ours. Each arcade's row has `ping_ms` (the link's round trip: the least of its last pongs; null when not linked) with `ping_via` (`link` or `relay`), and `machine` when it shares its system info. |
| `mesh.sysinfo` | `/api/v1/mesh/sysinfo` | public | — | The machines behind the mesh, as their operators share them (opt-in): every other arcade's newest signed record heard in the last 24 h (CPU, cores, RAM, GPUs with VRAM or `unified`, disk total and free, OS, architecture, version; `seal`: core-seconds per GiB, measured from its own seals), `sharing` of `heard` arcades, `totals` (cores, RAM, disk, GPUs and VRAM where known, fastest/median/slowest seal speed) and this arcade's own (`me`). Reported and rounded; never on a chain; nothing that pays reads it. |
| `sysinfo.status` | `/api/v1/sysinfo/status` | read | — | Whether this arcade shares its system info, and the exact record it sends or would send (`record`), its size, the seal measurement (`sealed_bytes` of the 64 MiB needed) and what is never shared. |
| `sysinfo.set` | `/api/v1/sysinfo/set` | admin | on | Share this arcade's system info with the mesh (`on: true`) or stop (`on: false`: an off record goes out and every arcade drops the machine at once). Off by default; kept across restarts and updates. |
| `explorer.arc_machines` | `/api/v1/explorer/arc_machines` | public | — | The same as `mesh.sysinfo`, for apps (also `/r/explorer/arc_machines`). A record's `node` is the key an earner has in `explorer.arc_holders`. |
| `explorer.arc_nodes` | `/api/v1/explorer/arc_nodes` | public | — | The Storage app's Nodes and Coins (also `/r/explorer/arc_nodes`): per arcade on the mesh and this one, its chains (mode, indexes, bytes with `size` measured / estimated / unknown), sealed, ready-to-seal-now (cut into segments) and listed-not-cut-yet bytes, `beta` for old arcades (they can't seal), its machine line when shared; `offmesh` owners of sealed copies not heard lately; per chain keepers, size, listed, segments, replicas, unheld; `totals`. Matched by node id only. Cached 10 s, at most 500 arcades. |

### Tx

| method | route | needs | params | what |
|---|---|---|---|---|
| `tx.coins` | `/api/v1/tx/coins` | public | addresses, chain? | Spendable coins of up to 500 addresses (confirmed, not spent by a waiting transaction, and unconfirmed outputs), to build a transaction for a browser to sign. Coins carrying an ord inscription, and coins of at most 0.01 that no ord index can vouch for, are listed under `held_back` instead (not in `total`), so a payment built from the list never spends an inscription. Pepecoin testnet or an installed chain, Addresses index on. On a borrowed chain (not kept here, or kept but still syncing or indexing, Pepecoin testnet included) the coins come from a lender's address index, each checked against its block and unspent at the lender (`"borrowed": true`). |
| `tx.activity` | `/api/v1/tx/activity` | public | addresses, since?, chain? | What changed for up to 500 addresses: history rows above height since, and waiting transactions paying or spending them; height to pass as since next time. |
| `tx.fee` | `/api/v1/tx/fee` | public | chain? | The fee rate (per 1000 bytes) and dust limit this arcade's wallet builds transactions with on that chain (kept or borrowed). |
| `tx.broadcast` | `/api/v1/tx/broadcast` | post | hex, chain? | Send a transaction signed elsewhere (a website user's browser): every input must spend a coin this arcade knows, outputs no more than inputs; pooled here and sent to the coin's network. On a borrowed chain it goes to the lenders' nodes, which check it; refused by all, it is not sent. |
| `tx.status` | `/api/v1/tx/status` | read | chain, txid \| client_ref | Where a transaction stands as this arcade can tell: state confirmed {height, block, confirmations}, accepted_local (in its pool; on the ARCADE chain broadcast {peers_linked, peers_sent, peers_holding, peers_acked, peers_refused}), broadcast_failed (refused here or by every peer that answered: broadcast.refusals [{peer, code, error}], code fee_too_low, conflict, missing_coin, dust, not_yet_valid, blob_missing, pool_full, invalid, rules or unreachable), signed (signed here: the receipt is written the moment it is signed, before any network step; in no pool or block yet), dropped (signed here, gone 30 min later) or unknown; ever_signed_here, error (what the send answered, when it failed after signing), and code not_found when this arcade never signed it. |
| `tx.wait` | `/api/v1/tx/wait` | read | chain, txid \| client_ref, until?, timeout? | tx.status, answered once the transaction reaches until (accepted_local, broadcast_ok, seen_in_peer_mempool, confirmed; default confirmed) or is dropped or broadcast_failed, or after timeout seconds (default 60, at most 600) with timed_out: true. |
| `tx.resend` | `/api/v1/tx/resend` | wallet | chain, txid \| client_ref | Send again a transaction a send here signed (it has a receipt: tx.status ever_signed_here) and this arcade still holds the bytes of, the way it first went out: the ARCADE chain's own pool (offered to the linked arcades from there), a borrowed chain's lenders, or a kept chain's pool and peers. By client_ref, every transaction of that send, the first first; by txid, the ones the same send signed before it, then it. Nothing is signed again. Answers tx.status's shape for it (by client_ref, the send's last), with resent [{txid, ok, how (pool, lenders, peers, in_block) \| error, code}]; one in a block already answers confirmed with resent []. Code not_found when nothing signed here matches; an error when it was signed here but its bytes are no longer kept. |

### Psbt

Build-only transactions for an account that signs in the ARCADE app (see "Account sign-in and payments" below). No
keys and nothing signed or sent: `address` and `pubkey` (hex, compressed) are the paired account's, from the pairing's
`home` (or its Dogecoin key with `chain: "doge"`); `chain` defaults to the chain the account's @tag is on (ARCADE testnet on the
test arcades). Coins are the ones `tx.coins` lists (never one carrying an inscription), the fee rate and dust
`tx.fee`'s, and records are byte for byte what `feed.post` and `feed.act` write. One transaction each; a record too long
for one is refused. At most 600 builds a minute per arcade, 60 per address.

Every builder answers
`{psbt, chain, unit, address, what, summary: {inputs: [{coin, value, from}], outputs: [{index, value, to, mine}], fee}}`:
`psbt` is base64 (each input carries its whole previous transaction), `to` an address or `data (OP_RETURN)`, `mine`
marks the change. Hand `psbt` to the app as `{type: "psbt", chain, psbt, finalize: true}`; its answer's `txHex` (or
`psbt.finalize` of its `psbtHex`) goes to `tx.broadcast`.

| method | route | needs | params | what |
|---|---|---|---|---|
| `psbt.send` | `/api/v1/psbt/send` | public | address, pubkey, chain?, to, amount | A payment: `amount` (smallest units, at least the dust) to `to`, an address of the chain or a @tag (paid where `wallet.send` would pay it), change back. |
| `psbt.feed_post` | `/api/v1/psbt/feed_post` | public | address, pubkey, chain?, text, channel?, file? | A public post (needs a @tag): `channel` such as `txtok` (default `main`, at most 24 characters), `file` = `{name, type, data}` (base64, at most 1 MB, and the whole record at most 7,646 bytes). |
| `psbt.feed_act` | `/api/v1/psbt/feed_act` | public | address, pubkey, chain?, kind, target, text?, amount? | A feed act on `target` (a post's or reply's txid): like, unlike, dislike, undislike, reply, share, edit, delete (`text` for reply, share and edit), or tip: `amount` paid to the post's author on that chain in the same transaction (their address there: the same key's on a chain that derives keys as the ARCADE chain does, else the one they proved). |
| `psbt.finalize` | `/api/v1/psbt/finalize` | public | psbt, chain? | A signed PSBT (base64 or hex): every P2PKH signature checked, a partial one finished; `{hex, txid, chain, size, fee}` for `tx.broadcast`. Refused when an input isn't signed or a signature doesn't check. |

### Id

| method | route | needs | params | what |
|---|---|---|---|---|
| `id.start` | `/api/v1/id/start` | public | host, path?, http? | ARCADE ID sign-in (no password; built on Digi-ID, created by the DigiByte community): a code for a website to show as a QR code, digiid://host/path?x=nonce (path default /digiid/callback; http: true for a plain-http test address). Digi-ID wallets answer it (Coinomi is the one digi-id.io names); an ARCADE wallet may answer with a key on any chain it holds. Lasts 5 minutes, works once. |
| `id.callback` | `/api/v1/id/callback` | public | address, uri, signature, chain? | What the wallet posted to the website's callback URL, checked: a code from id.start, not used or run out, signed by address (Core signmessage over uri: Digi-ID's DigiByte prefix, or with chain that chain's prefix or ARCADE's). |
| `id.poll` | `/api/v1/id/poll` | public | nonce | A sign-in code's state for the website's page: waiting, done (once: the address that signed and its chain; chain null = a Digi-ID wallet's key for this site), expired or unknown. The website links that address to its user. |
| `id.link_start` | `/api/v1/id/link_start` | public | key, host, via? | Add a device with no password: the new browser's one-time public key (hex; X25519 for the app's sender) in; a code, a 6-digit number and the QR text (arcadelink://host/code?k=key[&a=via]) out. `via`: this arcade's https address, for a site with no node of its own (the other device sends there). The user's other device scans it, shows the same number (the first 4 bytes of sha256("ARCADE/link\0" + code + "\0" + key), mod 1,000,000) and sends the 12 words sealed to that key. |
| `id.link_send` | `/api/v1/id/link_send` | public | code, sealed | The other device's sealed message for the new browser (sealed to its key: this arcade only passes it on), once per code. |
| `id.link_poll` | `/api/v1/id/link_poll` | public | code | The new browser collects what was sent: waiting, done (once: sealed and the key it is sealed to), expired or unknown. |
| `id.link_answer` | `/api/v1/id/link_answer` | local | qr, send? | The desktop app as the other device: an arcadelink:// text in. Without send: the site, the number to compare and the @tag that would go. With send: the active account's words sealed to the browser's key and posted to `<via or https://host>/api/v1/id/link_send`. Sealed = {v: 1, kem: "X25519", epk, nonce, ct} (hex): AES-256-GCM under sha256("ARCADE/link\0" + X25519(shared) + epk + the browser's key); inside, {words, tag}. |
| `id.box_put` | `/api/v1/id/box_put` | public | box, msg | A sealed message for the other end of an ARCADE ID signing pairing (box: 32 hex characters only the two ends know; up to 16 waiting, 1,500,000 characters each, 64 MB in all boxes, ten minutes). The arcade only passes it on. |
| `id.box_take` | `/api/v1/id/box_take` | public | box | Everything waiting in a box, oldest first; the box is emptied. |
| `id.sign_pair` | `/api/v1/id/sign_pair` | local | qr, send? | Pair a website with this app so it can ask the app to sign: an `arcadesign://site/<pairbox>?k=<page X25519 key>[&a=<arcade>]` code in. Without send: the site, the 6-digit number to compare, the account, its Dogecoin address and `home: {chain, name, address}`. With send: the app puts {v: 1, apk, n, c} in the pair box (inside: `{address, pubkey, tag, chain: "doge", home: {chain, address, pubkey}}`: the Dogecoin key m/44'/3'/0'/0/0 and the key on the chain its @tag is on (ARCADE)); both ends derive K = sha256("ARCADE/sign\0" + shared + app key + page key), the request box = first 16 bytes of sha256("ARCADE/sign/req\0" + K), the answer box likewise with "res"; {n, c} = AES-256-GCM under K (nonce hex, ciphertext base64). |
| `id.sign_sites` | `/api/v1/id/sign_sites` | local | — | Websites paired with this app. |
| `id.sign_forget` | `/api/v1/id/sign_forget` | local | id | Unpair a website. |
| `id.sign_pending` | `/api/v1/id/sign_pending` | local | — | What paired sites ask this app to sign (taken from their request boxes every 3 s): a message, or a PSBT (each signed input must carry its previous transaction, which must hash to its txid) with the coins it signs, every output, the fee and whether a signed coin carries an inscription (this arcade's ord index, else the mesh's). Requests inside: {id, type: "psbt", psbt, inputToSign?, finalize?, label?}, {id, type: "psbts", psbts: [{psbt, inputToSign?, finalize?, label?}]} (up to 50, signed together on one yes; the answer has `results`) or {id, type: "message", message, label?}. `label` is the site's own name for it, shown as the site's. Which key: the Dogecoin key, unless the request has `key: "home"` (a message) or `chain: "<the key of the chain its @tag is on>"` (a PSBT; `key: "home"` works too): then the account's key on that chain, a message signed with ARCADE's prefix, a PSBT shown in that chain's addresses and units, and every coin it signs must be an unspent coin of that chain here. Any other chain is refused; a `psbts` batch is on one chain. Each shows `key`, `chain`, `chain_name`, `unit` and `address` (the signing one). An inscription reveal is signed only when named in `inputToSign` with sighashType 1 (ALL): its coin is P2SH of the redeem script the PSBT carries (input key 0x04), that script is exactly `6b 6b`, 1–200 × `75`, `6c 6c`, `ab`, then the account's P2PKH; it is signed over that P2PKH (after OP_CODESEPARATOR) and left as a partial signature even with `finalize`, for the site to build the scriptSig. Such inputs show `reveal: true`. |
| `id.sign_decide` | `/api/v1/id/sign_decide` | local | id, ok | Sign a waiting request (Dogecoin key m/44'/3'/0'/0/0, or the account's key on the chain its @tag is on when the request names it; legacy P2PKH inputs of that key's, ALL, ALL\|ANYONECANPAY or SINGLE\|ANYONECANPAY; a named inscription reveal, ALL, partial signature only) or refuse; the answer {id, ok, psbtHex, txHex?} or {id, ok, signature, address} or {id, ok: false, error} goes to the answer box. |
| `id.answer` | `/api/v1/id/answer` | local | code, chain?, send? | Sign in to a Digi-ID site with this arcade's wallet (the active account), as a phone wallet such as Coinomi does: a digiid:// code in. Without send: the site's name, where the answer goes and this wallet's key for that site (SLIP-0013, never a key that holds coins; the same one the web wallet makes from the same words), to show first. With send: signed and sent to the site. chain: dgb (Digi-ID, the default) or another chain this wallet holds (ARCADE ID). |
| `id.relay` | `/api/v1/id/relay` | public | uri, address, signature, chain? | A website's user signed a Digi-ID code in the browser; the site's server passes the answer here and the arcade sends it to the code's site (a page may not post to another site; a server may). Only a well-formed code, signed by that address, once, and only to a public internet address (never this computer or its network; no redirects). |

### Vault

| method | route | needs | params | what |
|---|---|---|---|---|
| `vault.put` | `/api/v1/vault/put` | public | tag, address, pubkey, blob, signature | Password accounts: keep @tag's sealed record here (the 12 words sealed under the password in the browser: {kdf: PBKDF2-SHA512, iterations, salt, nonce, sealed}; nothing here can open it). address = the one @tag names on chain, or for a free @tag an address on the chain @tags are kept on (ARCADE): the record is then kept as a reservation (first come) until the @tag is claimed on chain, and a claim by anybody else ends it; signature = its Core signmessage (ARCADE's prefix) over "ARCADE vault @tag address pubkey sha256", sha256 of the record as {kdf, iterations, salt, nonce, sealed} JSON. Replaces an older one. |
| `vault.get` | `/api/v1/vault/get` | public | tag, here? | @tag's sealed record {tag, address, pubkey, blob, from?}: kept here, else (unless here) from the arcade that holds it. The shape of the original arcades' /signin/<tag>: a website serving it there lets other arcades find accounts made on it. |
| `vault.delete` | `/api/v1/vault/delete` | public | tag, signature | Drop @tag's sealed record here ("stop using a password"): signature = @tag's address signing "ARCADE vault delete @tag address sha256" of the record kept. The account then opens on this site with ARCADE ID or its 12 words. |

### Wallet

| method | route | needs | params | what |
|---|---|---|---|---|
| `wallet.create` | `/api/v1/wallet/create` | local | phrase? | Create a wallet (or restore one from 12 words). Returns the new words once. |
| `wallet.phrase` | `/api/v1/wallet/phrase` | local | — | The wallet's 12 words. |
| `wallet.keys` | `/api/v1/wallet/keys` | read | chain? | Your addresses on a chain, their @tags and balances (on SegWit chains each address type's address and balance under kinds; balance counts every type). balance follows the mempool (confirmed coins less those a waiting transaction spends, plus waiting outputs to you); confirmed is the blocks' figure; free the confirmed coins spendable now (not spent by a waiting transaction, mature, no inscription); pending / pending_coins what is arriving; waiting and dropped this wallet's transactions in the mempool and those that left it without a block. |
| `wallet.addr_kind` | `/api/v1/wallet/addr_kind` | wallet | chain, kind? | The address type the Wallet shows on a chain (legacy, p2sh-p2wpkh, p2wpkh, p2tr; remembered per chain) and the types it offers; kind: remember that one. |
| `wallet.coins` | `/api/v1/wallet/coins` | read | chain?, from?, kind?, all_kinds? | Coin control: the coins a wallet.send with the same from, kind and all_kinds could spend (value, address, confirmations, age; status free, inscription or unchecked (small and nobody could check: kept back); frozen; spendable), with the chain's fee rule (fee_per_kb, per_started_kb, dust) and the change address (from). |
| `wallet.freeze` | `/api/v1/wallet/freeze` | wallet | chain, coins, frozen? | Coin control: freeze coins (frozen true, the default) so no automatic send spends them and they can't be picked, or unfreeze them (false). The chain's frozen coins after. |
| `wallet.coin_prefs` | `/api/v1/wallet/coin_prefs` | wallet | coin_control?, tip? | Coin control's setting for the account in use (off until turned on) and whether its how-to tip shows; with either, change it first. |
| `wallet.send` | `/api/v1/wallet/send` | local | chain?, to, amount, from?, kind?, all_kinds?, send?, client_ref?, coins? | client_ref (with send): as for ledger.item_open. coins (coin control): spend exactly these coins ("txid:vout", from wallet.coins), all of them and no other; refused when one is frozen, kept back for an inscription or spent, or they don't pay the amount and fee. Pay amount (satoshis) on a chain (Pepecoin testnet by default, or any installed or borrowed one) from this wallet (from: one of its addresses or an imported wallet's; otherwise the own address with the most), signed here; kind: the address type it's sent from, change back to it (default: from's type, else the one the Wallet shows); all_kinds: also spend the wallet's other address types' coins; send: true broadcasts, otherwise it's only built and shown with its fee (and a warning when the address could be meant for another chain). |
| `wallet.link` | `/api/v1/wallet/link` | wallet | chain, wif, tag | Link another wallet to your @tag (signed by both keys). |
| `wallet.unlink` | `/api/v1/wallet/unlink` | wallet | chain, address, tag | Remove a linked wallet. |
| `wallet.links` | `/api/v1/wallet/links` | read | chain | Your linked wallets on a chain. |
| `wallet.import` | `/api/v1/wallet/import` | local | chain, secret, addr_kind?, name? | Import a wallet: a WIF key, or another wallet's words (one address, index 0); addr_kind legacy (default), p2sh-p2wpkh or p2wpkh. Kept encrypted here; nothing is published. |
| `wallet.imports` | `/api/v1/wallet/imports` | local | — | Imported wallets, with balances on chains this arcade keeps addresses for. |
| `wallet.import_remove` | `/api/v1/wallet/import_remove` | local | chain, address | Forget an imported wallet and its key. |
| `wallet.requests` | `/api/v1/wallet/requests` | read | page? | Requests from pages to send, waiting for your answer. |
| `wallet.preview` | `/api/v1/wallet/preview` | read | id | A request's transaction as it would be built now: outputs and fee. |
| `wallet.approve` | `/api/v1/wallet/approve` | local | id | Approve a request: sign and send it. |
| `wallet.deny` | `/api/v1/wallet/deny` | local | id | Refuse a request. |

### Dd

| method | route | needs | params | what |
|---|---|---|---|---|
| `dd.wallet` | `/api/v1/dd/wallet` | read | chain? | Your DigiDollar on DigiByte (chain dgb, the default, or dgbtest): address (DD.../TD...), cents, coins, borrowed, whole (false when only coins from the borrow floor on can be seen). |
| `dd.send` | `/api/v1/dd/send` | local | chain?, to, cents, send?, client_ref? | Send DigiDollars (cents, $1 to $100,000 per send) to a DD.../TD... address, with DGB from the same wallet for the fee (0.35 DGB/kB, at least 0.1, at most 0.5 DGB); send: true broadcasts, otherwise it's only built and shown. |

### Account

| method | route | needs | params | what |
|---|---|---|---|---|
| `account.signin` | `/api/v1/account/signin` | local | tag, password, from? | Sign in with an existing Dogecoin Arcade account (@tag + password): its words come from the arcade that holds it (from, or any we know), sealed under the password, then its imported wallets and mailbox. The account becomes the active one. |
| `account.list` | `/api/v1/account/list` | read | — | Accounts on this arcade: its own wallet first, then signed-in accounts; which one is active. |
| `account.switch` | `/api/v1/account/switch` | local | id | Use another account ("" = this arcade's own wallet): wallet, messages, groups and rooms follow it. |
| `account.signout` | `/api/v1/account/signout` | local | id | Sign an account out of this arcade: its words, keys, messages and groups here are forgotten. |

### Tags

| method | route | needs | params | what |
|---|---|---|---|---|
| `tag.check` | `/api/v1/tag/check` | public | tag, chain? | Whether a @tag is free and valid on one chain (every chain has its own @tags; see "@tags per chain" below). tag may name the chain (`alice.doge`), or chain does; else where new tags go. Answers chain, full (`alice.doge`) and label (`@alice · DOGE`). |
| `tag.claim` | `/api/v1/tag/claim` | wallet | tag, chain?, client_ref? | Claim a @tag on one chain: ARCADE, Pepecoin testnet (test coins asked for by themselves), or a mainnet chain tags live on here (pep, doge, dgb: kept with their ledger or borrowed; paid in that coin). tag may name the chain (alice.doge). |
| `tag.resolve` | `/api/v1/tag/resolve` | public | tag | What an @tag names: {found: one / many / none, choices: [{tag, chain, label, address, name}]}. many = different people hold the name on several chains: show them and let the person pick. |
| `tag.list` | `/api/v1/tag/list` | read | — | Your tag claims and their progress. |

### Faucet

| method | route | needs | params | what |
|---|---|---|---|---|
| `faucet.ask` | `/api/v1/faucet/ask` | wallet | index?, tag? | Ask the mesh for test coins. |
| `faucet.arc` | `/api/v1/faucet/arc` | wallet | address | The user faucet: ask the mesh to top a website user's ARCADE address up to 0.05 ARCADE, in two coins (once per address a day; a website calls it when a user signs in). This arcade's own wallet gets 1 ARCADE once from the node faucet instead. |
| `faucet.arc_status` | `/api/v1/faucet/arc_status` | read | — | The ARCADE top-up asks this arcade knows and how many it paid today; `node_faucet`: node-gift asks, gifts paid today, nodes known paid, and this arcade's own gift. |
| `faucet.newcomers` | `/api/v1/faucet/newcomers` | read | — | New arcades waiting for their welcome (1 ARCADE, so they can claim a @handle at once), this arcade's welcome settings, whether it can send one now, and the welcomers' high scores (who welcomed each new arcade first, read from the chain). |
| `faucet.welcome` | `/api/v1/faucet/welcome` | wallet | node, amount? | Welcome a new arcade now from this wallet ("Welcome to the arcade"): this arcade's welcome amount, or amount (1 to 100 ARCADE, in satoshis). Refused once any arcade has welcomed it; the first payment on the chain gets the high-score point. |
| `faucet.welcome_set` | `/api/v1/faucet/welcome_set` | admin | auto?, amount? | Welcome new arcades automatically (on unless turned off; after a 3-minute head start for people pressing the button) and the amount each gets (in satoshis: 1 to 100 ARCADE, 1 ARCADE unless set). |

### Messages

| method | route | needs | params | what |
|---|---|---|---|---|
| `msg.me` | `/api/v1/msg/me` | read | — | Your messaging key, fingerprint and whether it is published. |
| `msg.stats` | `/api/v1/msg/stats` | public | — | How many message candidates, posts, acts and machine records the ARCADE chain holds (Pepecoin testnet's when ARCADE can't be used here). |
| `msg.scan` | `/api/v1/msg/scan` | message | — | Look for new messages now (it also runs every 15 seconds). |
| `msg.who` | `/api/v1/msg/who` | public | name | Who a @tag or address is, and their published messaging key. |
| `msg.verify` | `/api/v1/msg/verify` | public | address, message, signature, prefix?, chain? | Whether `signature` (Core signmessage: 65-byte compact, base64) is `address`'s over `message`, with `prefix`, else `chain`'s message prefix, else ARCADE's (`Pepecoin Signed Message:\n`, what the ARCADE app signs with the account's ARCADE key). Answer `{valid, address, prefix}`. |
| `msg.conversations` | `/api/v1/msg/conversations` | read | requests? | Conversations, newest first (requests: people not in your contacts). |
| `msg.thread` | `/api/v1/msg/thread` | read | peer | Every message with one person (their messaging key). |
| `msg.read` | `/api/v1/msg/read` | message | peer | Mark a conversation read. |
| `msg.unread` | `/api/v1/msg/unread` | read | — | Unread counts: messages, requests, group lines. |
| `msg.file` | `/api/v1/msg/file` | read | id | A message's file (base64). |
| `msg.send` | `/api/v1/msg/send` | message | to, text?, file?, client_ref? | Send a private message to a @tag, address or key; file = {name, type, data (base64)}. |
| `msg.delete` | `/api/v1/msg/delete` | message | id | Stop showing a message here (the chain keeps it). |
| `msg.announce` | `/api/v1/msg/announce` | wallet | client_ref? | Publish your messaging key now if it isn't on chain. |
| `profile.publish_addresses` | `/api/v1/profile/publish_addresses` | wallet | send?, client_ref? | List this wallet's own addresses (Pepecoin mainnet, Dogecoin, each other coin installed here) on your @tag's profile, keeping the rest of it; no private key is pasted. send: true broadcasts, otherwise it's only built and shown with its fee and total cost. |

### Contacts

| method | route | needs | params | what |
|---|---|---|---|---|
| `contacts.list` | `/api/v1/contacts/list` | read | — | Your contacts. |
| `contacts.save` | `/api/v1/contacts/save` | message | key, name?, address?, notes? | Add or change a contact (by messaging key). |
| `contacts.delete` | `/api/v1/contacts/delete` | message | key | Remove a contact. |

### Mutes

| method | route | needs | params | what |
|---|---|---|---|---|
| `mute.list` | `/api/v1/mute/list` | read | — | Muted addresses. |
| `mute.add` | `/api/v1/mute/add` | message | address | Mute an address (messages and posts). |
| `mute.remove` | `/api/v1/mute/remove` | message | address | Unmute an address. |

### Groups

| method | route | needs | params | what |
|---|---|---|---|---|
| `group.list` | `/api/v1/group/list` | read | — | Your groups, with their last line and unread count. |
| `group.create` | `/api/v1/group/create` | message | name, members, description? | Make a private group (members: @tags or addresses) and invite them. |
| `group.thread` | `/api/v1/group/thread` | read | id | A group's lines, members and extras. |
| `group.read` | `/api/v1/group/read` | message | id | Mark a group read. |
| `group.say` | `/api/v1/group/say` | message | id, text?, file? | Write to a group. |
| `group.leave` | `/api/v1/group/leave` | message | id | Leave a group. |
| `group.app_list` | `/api/v1/group/app_list` | read | — | Your groups as apps see them: id, name, live room, members. |
| `group.app_read` | `/api/v1/group/app_read` | read | id, after?, limit? | A group's lines for an app (oldest first). |
| `group.seal` | `/api/v1/group/seal` | read | id, text | Seal text for a group's live room (the original's group live key). |
| `group.open` | `/api/v1/group/open` | read | id, sealed | Open text sealed for a group's live room (null if not this group's). |
| `group.add` | `/api/v1/group/add` | message | id, members | Add members to a group (they get its key). |
| `group.update` | `/api/v1/group/update` | message | id, changes | Admins: change admins, description, picture or pinned message. |
| `group.remove` | `/api/v1/group/remove` | message | id, members | Admins: remove members (gives everyone else a new group key). |

### Feed

| method | route | needs | params | what |
|---|---|---|---|---|
| `feed.channels` | `/api/v1/feed/channels` | public | — | Channels and how many posts each has. |
| `feed.settings` | `/api/v1/feed/settings` | read | set? | Feed settings (hide muted, contacts only); set = {hide_muted, contacts_only} needs post. |
| `feed.post` | `/api/v1/feed/post` | post | text?, channel?, file?, client_ref? | Post publicly (needs a @tag). |
| `feed.act` | `/api/v1/feed/act` | post | kind, target, text?, amount?, client_ref? | Like, unlike, dislike, undislike, reply, share, edit, delete; tip (needs tip; amount in satoshis). |
| `feed.file` | `/api/v1/feed/file` | public | txid | A post's file (base64). |

### Ord

| method | route | needs | params | what |
|---|---|---|---|---|
| `ord.chunks` | `/api/v1/ord/chunks` | public | from?, chain? | Ord fingerprint chunks (as other arcades compare them). |
| `ord.send` | `/api/v1/ord/send` | local | inscription, to, chain?, send?, client_ref? | Send one inscription (id or number) from this wallet's address on a chain kept here with its ord index, or borrowed (where it sits from the arcades keeping the ord index, which must agree; coins from a lender, checked against their blocks; sent through the lenders): its coin whole to `to` first, clean coins pay the fee. Only one alone at the start of its coin. send: true broadcasts, otherwise it is built and shown with its fee. |

### Explorer

| method | route | needs | params | what |
|---|---|---|---|---|
| `explorer.summary` | `/api/v1/explorer/summary` | public | chain? | Chain summary for the explorer's front page. |
| `explorer.earn` | `/api/v1/explorer/earn` | public | chain? | This arcade's ARCADE pay, read only (docs/EARN.md): its turn, next payout, accrued pay, pay and draw weight, why pay waits, and its sealed replicas waiting to register; with a chain, that coin's Earn ARCADE progress too. App pages read it at `/r/explorer/earn`. |
| `explorer.blocks` | `/api/v1/explorer/blocks` | public | chain?, offset?, limit?, before?, n?, order?, from? | Recent blocks, newest first. offset (back from the tip) and limit (at most 100) answer a page: {items, offset, limit, limit_applied (when the limit asked for was cut), next (the offset of the next page; null at block 0), chain, as_of, source}. before and n (at most 100) answer the plain list, as before. order asc with from (a height, default 0): a feed oldest first, {items, from, limit, next_from (the height to ask from next; the tip + 1 at the end: chains.wait waits for it), tip, chain, as_of, source}. |
| `explorer.block` | `/api/v1/explorer/block` | public | chain?, id | One block (id: a height, as a number or text, or a hash). Carries chain, as_of {height, hash} (the tip its confirmations count against), source (local, borrowed or mesh), unit and decimals. |
| `explorer.tx` | `/api/v1/explorer/tx` | public | chain?, txid | One transaction: height, block (its hash), confirmations, fee, inputs and outputs in base units (decimals: 8); an OP_RETURN output carries data (hex) and decoded (an arcade record: repository or A:// with its op). Carries chain, as_of, source; without chain it answers on Pepecoin testnet and says so (chain_note). |
| `explorer.address` | `/api/v1/explorer/address` | public | chain?, address, history?, inscriptions?, content?, history_offset?, history_limit? | An address: balance, history (newest first, a page at a time: history_offset (or offset) and history_limit (or limit; default 50, at most 500); history_next is the next page's offset, null after the oldest; tx_count the total), inscriptions (the first 60; all with explorer.held), tokens. history: false or inscriptions: false leaves that part out; content: false lists inscriptions without previews. pending: the balance with the mempool taken in, what arrives and leaves, and the waiting transactions (when the mempool touches the address). |
| `explorer.inscriptions` | `/api/v1/explorer/inscriptions` | public | chain?, offset?, limit? | Inscriptions, newest first. |
| `explorer.inscription` | `/api/v1/explorer/inscription` | public | chain?, id | One inscription. |
| `explorer.tokens` | `/api/v1/explorer/tokens` | public | chain?, offset?, limit? | Tokens. |
| `explorer.token` | `/api/v1/explorer/token` | public | chain?, tick | One token. |
| `explorer.names` | `/api/v1/explorer/names` | public | chain? | Names. |
| `explorer.name` | `/api/v1/explorer/name` | public | chain?, name | One name. |
| `explorer.maps` | `/api/v1/explorer/maps` | public | chain? | Maps. |
| `explorer.map` | `/api/v1/explorer/map` | public | chain?, map | One map. |
| `explorer.mempool` | `/api/v1/explorer/mempool` | public | chain? | Waiting transactions. |
| `explorer.ledger` | `/api/v1/explorer/ledger` | public | chain? | Arcade ledger summary. |
| `explorer.ledger_tokens` | `/api/v1/explorer/ledger_tokens` | public | chain?, offset?, limit? | Arcade ledger tokens. |
| `explorer.ledger_token` | `/api/v1/explorer/ledger_token` | public | chain?, id | One arcade ledger token. |
| `explorer.ledger_inscriptions` | `/api/v1/explorer/ledger_inscriptions` | public | chain?, offset?, limit? | Arcade ledger inscriptions. |
| `explorer.ledger_inscription` | `/api/v1/explorer/ledger_inscription` | public | chain?, id | One arcade ledger inscription. |
| `explorer.tag` | `/api/v1/explorer/tag` | public | chain?, tag | A @tag's page: addresses (with linked and imported wallets) and balances. |
| `explorer.search` | `/api/v1/explorer/search` | public | chain?, q | Search: block, tx, address, @tag, inscription, token, name. |
| `explorer.inscription_types` | `/api/v1/explorer/inscription_types` | public | chain? | How many ord inscriptions of each kind (images, text, HTML, ...) a chain holds. |
| `explorer.site` | `/api/v1/explorer/site` | public | chain?, name | A name's site as this arcade's names index holds it: {name, claim, holder, page, scanning}. |
| `explorer.sites` | `/api/v1/explorer/sites` | public | chain?, all? | Sites by name, newest first: {names: [{name, page, number, height, title}], scanning}. |
| `explorer.ledger_bytes` | `/api/v1/explorer/ledger_bytes` | public | chain?, id | An arcade inscription's whole bytes (base64, up to 16 MB), for an arcade that does not keep this chain's ledger: {txid, number, content_type, sha256, b64}. |
| `explorer.page` | `/api/v1/explorer/page` | public | chain?, path, query? | A public page read (GET /r/...) answered on this chain, for an app opened at an arcade that does not keep its ledger. |
| `explorer.app_cards` | `/api/v1/explorer/app_cards` | public | chain? | The rows of a chain that may be app cards, for an arcade's App Store that borrows it: {rows}. |
| `explorer.ord_outpoints` | `/api/v1/explorer/ord_outpoints` | public | chain?, outpoints | Which of these outpoints carry ord inscriptions (outpoints: ["txid:vout", ...]). |
| `explorer.token_note` | `/api/v1/explorer/token_note` | public | chain?, id | The token note an inscription carries (PRC-20 / DRC-20): {id, note}. |
| `explorer.prize_rows` | `/api/v1/explorer/prize_rows` | public | chain? | The prize pool rows of a chain's ledger: {rows}. |
| `explorer.activity` | `/api/v1/explorer/activity` | public | chain?, addresses, since? | tx.activity read from this arcade's own copy (never borrowed): history rows and waiting transactions of up to 500 addresses. |

### Chains

| method | route | needs | params | what |
|---|---|---|---|---|
| `chains.peers` | `/api/v1/chains/peers` | read | key | A chain's P2P address book: nodes known, working in the last day, resting after failures; the working ones and the next to try. |
| `chains.list` | `/api/v1/chains/list` | public | — | Every chain V1.0 knows, installed or not, with sizes reported by arcades; per chain: hidden (chains.hide), queued (position), queued_resume and installing (the chain being installed now). |
| `chains.status` | `/api/v1/chains/status` | public | key | One chain's staged progress. `recovered_at_start` is `{from, to}` when this start wound the tip back (the last stop left without writing the coin caches, exit code 3), else null. |
| `chains.offers` | `/api/v1/chains/offers` | public | — | Chains other arcades offer over the mesh. |
| `chains.sizes` | `/api/v1/chains/sizes` | admin | — | Measure this arcade's disk use per chain now. |
| `chains.add` | `/api/v1/chains/add` | admin | key, mode?, with? | Install a chain (mode full, shared or chain; with = indexes). |
| `chains.queue` | `/api/v1/chains/queue` | public | — | Chains waiting in the queue, next first (position, key, name, mode, with; resume = a chain paused by chains.start_now that continues at its turn), the chain being installed now that they wait behind, the hidden chains, and plan_done (whether the startup chooser was answered). |
| `chains.unqueue` | `/api/v1/chains/unqueue` | admin | key | Take a chain out of the install queue. |
| `chains.queue_first` | `/api/v1/chains/queue_first` | admin | key | Move a queued chain to the front of the queue: it is next. |
| `chains.start_now` | `/api/v1/chains/start_now` | admin | key | Start a queued chain now: the chain being installed is paused and goes back into the queue right after it (a resume entry). Refused while Pepecoin testnet or the ARCADE chain is catching up. |
| `chains.hide` | `/api/v1/chains/hide` | admin | key, hidden? | Leave a chain out of the Coins & chains list (hidden: true, the default) or show it again (false). Display only: an installed chain keeps running; nothing is removed or paused. Pepecoin testnet and the ARCADE chain are always shown. |
| `chains.plan` | `/api/v1/chains/plan` | admin | keys?, mode?, skip? | The startup chooser: keys = the chains to use. Every other chain is hidden; the chosen ones not installed or queued yet are queued, smallest reported size for mode (full, shared or chain; default full) first, unknown sizes last, with no indexes. The ARCADE chain (arctest) is installed when it is among keys and syncs first; left out, it stays borrowed from arcades that keep it. Never removes a chain. skip: true only marks the chooser as answered. |
| `chains.pause` | `/api/v1/chains/pause` | admin | key | Pause a chain (by hand: the queue never resumes it). |
| `chains.resume` | `/api/v1/chains/resume` | admin | key | Resume a chain (by hand: a resume entry for it leaves the queue). |
| `chains.index_add` | `/api/v1/chains/index_add` | admin | key, index | Add an index to a chain. |
| `chains.index_remove` | `/api/v1/chains/index_remove` | admin | key, index | Remove an index from a chain. |
| `chains.remove` | `/api/v1/chains/remove` | admin | key | Remove a chain and its data. |
| `chains.copy_status` | `/api/v1/chains/copy_status` | public | key? | Core-data copies (cb) linked arcades offer for installed chains, the fetch and import in progress, and whether a chain's coin set is trusted from a copy or verified. |
| `chains.copy_start` | `/api/v1/chains/copy_start` | admin | key, what? | Bootstrap a chain from the best copy offered: what = coins (the coin set; missing blocks are fetched from the network and stored, default) or all (the copy's blocks too). |
| `chains.copy_from_dir` | `/api/v1/chains/copy_from_dir` | admin | key, dir | Bootstrap a chain once from a Core datadir on this computer (dir: the folder holding blocks/ and chainstate/, e.g. an old arcade's Dogecoin Core), instead of a copy fetched over the mesh. Read in place and never written; refused while that Core runs (stop it first), if pruned, or of another chain. Taken in as an all copy (headers checked by proof of work, every coin against our blocks), trusted until re-checked from the blocks in the background. This arcade keeps its own database afterwards: the folder can be removed later. |
| `chains.copy_cancel` | `/api/v1/chains/copy_cancel` | admin | key | Stop fetching a copy (its pieces are deleted) or decline the offer: the chain syncs the usual way. |
| `chains.refill` | `/api/v1/chains/refill` | admin | key, from?, to?, sources? | Fetch back block bodies lost here (pruned, and no sealed copy answers for them) between from and to (default: the whole chain), 50 at a time: a batch is taken only when at least sources (default 2, at most 5) independent sources (different node ids and IP addresses: arcades holding the blocks, arcades lending their Core, coin P2P peers) sent the same bytes, each block matching our header, merkle root and stored txids. Runs in the background; see chains.refill_status. |
| `chains.refill_status` | `/api/v1/chains/refill_status` | public | key? | Block refills (chains.refill): state, heights from/to/at, lost bodies found, restored, batches that couldn't be agreed, and the sources that agreed last. |
| `chains.refill_cancel` | `/api/v1/chains/refill_cancel` | admin | key | Stop a block refill (what it restored stays). |
| `chains.copy_verify` | `/api/v1/chains/copy_verify` | admin | key | Re-check a coin set taken from a copy: derive it from the blocks again (ours replaces it if it differs). |
| `chains.index_copy_status` | `/api/v1/chains/index_copy_status` | public | key? | Index copies (docs/INDEXBOOT.md): what old arcades offer per chain, the fetch, its votes, and whether an index here came from a copy. |
| `chains.index_copy_start` | `/api/v1/chains/index_copy_start` | admin | key, index, from? | Fetch an index copy now from the best offer (index = ledger or ord; from = one arcade's node id): it is taken in only if 2 or more other arcades agree with it. |
| `chains.index_copy_cancel` | `/api/v1/chains/index_copy_cancel` | admin | key, index | Stop fetching an index copy (its pieces are deleted). |
| `chains.wait` | `/api/v1/chains/wait` | public | chain, height, timeout? | Answered once chain's tip (as this arcade reads it: own copy or borrowed) reaches height, or after timeout seconds (default 60, at most 600): {chain, height, tip: {height, hash}, source, reached, timed_out}. |

### Share

| method | route | needs | params | what |
|---|---|---|---|---|
| `share.held` | `/api/v1/share/held` | public | — | V1.0 copies of chain parts held here (blocks, coins, ord, tokens, names, maps, ledger, profiles, messages): id, height, rows, size; and those being made. |
| `share.offers` | `/api/v1/share/offers` | public | key | V1.0 copies other arcades hold for a chain, per part: id, height, how many made it themselves and how many hold it. |
| `share.fetch` | `/api/v1/share/fetch` | admin | key, fam, id? | Fetch a part's copy from every arcade holding it at once: the newest one 2 or more arcades made themselves (or the id given). Pieces are checked against the manifest; the copy is then held and served here too. |
| `share.install` | `/api/v1/share/install` | admin | key | Install an added chain (or Pepecoin testnet) from V1.0 copies: its blocks and coins made by 2 or more arcades at the same checkpoint, fetched from every holder and taken in as they come (no replay); the coins are trusted until re-checked. |
| `share.install_cancel` | `/api/v1/share/install_cancel` | admin | key | Stop installing a chain from copies. |
| `share.cancel` | `/api/v1/share/cancel` | admin | key, fam | Stop fetching a copy (its pieces are deleted). |
| `share.auto` | `/api/v1/share/auto` | admin | key, on | Make copies of every part of a chain at each checkpoint (about a day of its blocks) while at the tip; off by default (copies take disk). |
| `share.make` | `/api/v1/share/make` | admin | key, fam | Make a V1.0 copy of one part of an installed chain now, from one frozen view of the database (in the background; see share.held). |

### Borrow

| method | route | needs | params | what |
|---|---|---|---|---|
| `borrow.status` | `/api/v1/borrow/status` | read | — | Borrowed coins: the switches, and per chain its lenders, tip, floor, tip mismatch and your balance. |
| `borrow.set` | `/api/v1/borrow/set` | admin | on, chain? | Turn borrowing on or off: for every coin (no chain) or for one chain (off removes its borrowed data). |
| `borrow.coins` | `/api/v1/borrow/coins` | read | chain | Your coins on a borrowed chain (from blocks read since the floor, and the lender's coin scan). |
| `borrow.send` | `/api/v1/borrow/send` | local | chain, to, amount, from?, kind?, all_kinds?, send?, client_ref? | Pay amount (satoshis) on a borrowed chain, signed here (kind, all_kinds: as wallet.send); send: true broadcasts to every lender, otherwise only built and shown. |

### Mining

| method | route | needs | params | what |
|---|---|---|---|---|
| `mine.start` | `/api/v1/mine/start` | admin | threads? | Mine Pepecoin testnet. |
| `mine.stop` | `/api/v1/mine/stop` | admin | — | Stop mining. |
| `mine.status` | `/api/v1/mine/status` | read | — | Mining state and hash rate. |

### Apps

| method | route | needs | params | what |
|---|---|---|---|---|
| `apps.store` | `/api/v1/apps/store` | public | sort?, q?, flagged? | The App Store: app cards from every chain with the arcade ledger (popular or new; starred first). Each card's `trust` {level, holders, checked, note}: green = this arcade's ledger or 2+ arcades alike, yellow = one arcade's word. |
| `apps.installed` | `/api/v1/apps/installed` | read | — | Apps installed on this arcade, each at its newest version. |
| `apps.install` | `/api/v1/apps/install` | admin | chain, txid | Install an app (it follows its author's updates). |
| `apps.uninstall` | `/api/v1/apps/uninstall` | admin | id | Uninstall an app. |
| `apps.star` | `/api/v1/apps/star` | admin | chain, txid, on? | Star or unstar an app (starred show first). |
| `apps.card` | `/api/v1/apps/card` | public | chain, txid | One app: card, versions, comments, permission switches. |
| `apps.perms` | `/api/v1/apps/perms` | admin | id, set? | An app's permission switches (wallet, contacts, groups, media, pictures, notify). |
| `apps.review` | `/api/v1/apps/review` | admin | chain, txid, verdict | Clear or confirm an app flagged as a lookalike. |
| `apps.trust` | `/api/v1/apps/trust` | admin | address, on? | Trust a publisher (never flagged as a lookalike). |
| `apps.storage` | `/api/v1/apps/storage` | local | page, op, key?, value? | An app page's saves (load, set, remove, clear, history), as arcade.storage. |
| `apps.owner` | `/api/v1/apps/owner` | read | page | arcade.owner.identity: whether this wallet created the page and still holds it. |
| `apps.owner_send` | `/api/v1/apps/owner_send` | local | page, request | arcade.owner.send: a page this wallet created and holds sends at once (testnet). |
| `apps.node` | `/api/v1/apps/node` | local | op, page, to?, body?, after?, limit? | arcade.node for a page: identity, send (sealed node-to-node), sent, replies. |
| `apps.launch` | `/api/v1/apps/launch` | read | chain, txid | What an app window opens: the sandboxed page URL and the card; `caution` when the version that opens is one arcade's word; `withdrawn` {reason, replaced_by} when its publisher withdrew it (it still opens). |
| `apps.publish` | socket only for `file` | wallet | file \| content_b64, type, name, card?, chain?, as?, send?, client_ref? | Inscribe an app page, icon or any file (4 MB at most) with this arcade's own wallet key, the same bytes `arcade-publish --name … --type … --json <card> <file>` writes. ARCADE by default. Without `send`: the plan and its cost. A repeat with the same `client_ref` finishes an unfinished publication and never pays twice. Answers {id (the first txid, served at /app/<chain>/<id>), txids, chunks, fee}. |
| `apps.withdraw` | `/api/v1/apps/withdraw` | wallet | chain?, txid \| family, reason?, replaced_by?, versions?, as?, send?, client_ref? | Withdraw one of this wallet's apps: a new version whose card says `"withdrawn": true` (with `reason`, `replaced_by`, and `versions` to take back only those). The store stops offering it everywhere; installed copies still open and say so. A family its own publisher withdrew whole no longer makes a new app of that name a lookalike. Refused unless this wallet published the app. An app published with a key file withdraws the same way: `arcade-publish --json '{"app": {"name": "<same name>", "family": "<same family>", "withdrawn": true, "reason": "…", "replaced_by": "…"}}' --name withdrawn.html --type text/html withdrawn.html`. |

### Realtime

| method | route | needs | params | what |
|---|---|---|---|---|
| `realtime.join` | `/api/v1/realtime/join` | local | page, room, game?, member?, cred?, fast? | Join a room over the mesh as this arcade's player (game defaults to the page): a token, me and the members; fast: true also takes the fast lane (realtime.fast). A website joins for its user with member (their address, or a guest id) and cred {a, x, s} signed over realtime.cert's words. |
| `realtime.ice` | `/api/v1/realtime/ice` | local | tag?, lan? | ICE servers for one player (tag tells players apart for the relay's per-login limit): a login on this arcade's media relay where other arcades proved its STUN port open (`lan: true` adds its LAN address, for a player on its own network); relays borrowed from linked arcades when it has none of its own. What a website hands its pages for WebRTC. |
| `realtime.cert` | `/api/v1/realtime/cert` | public | address | The words an address signs (Core signmessage) to play through this arcade, and their expiry: a website's user signs them in the browser and the site passes {a: address, x: expires, s: signature} to realtime.join. |
| `realtime.send` | `/api/v1/realtime/send` | local | token, data?, b64? | Send to everyone in the room: text or base64 bytes, at most 512 bytes, about 5 a second. |
| `realtime.leave` | `/api/v1/realtime/leave` | local | token | Leave the room. |
| `realtime.poll` | `/api/v1/realtime/poll` | local | token, wait? | Room events for this seat (join, leave, message), waiting up to wait ms (15000 at most); a seat not polled for 30 s is let go. |
| `realtime.fast` | `/api/v1/realtime/fast` | local | token, to, data?, b64?, reliable? | Send to some players of the room over the fast lane between arcades (a seat joined with fast): text or base64 bytes, at most 16 KB, about 240 a second; to names the players (up to 64). Reaches those who play here or through an arcade with a lane up (realtime.lanes); unreliable sends waiting to go out are replaced by the newest, reliable ones keep their order. Heard as fast events. |
| `realtime.media` | `/api/v1/realtime/media` | local | token, to, b64 | A piece of an app window's (or a website viewer's) encoded camera, microphone or screen (up to 60 KB) for some players of the room, over the direct link to their arcade, else through at most two arcades on the room's route that have their media relay on; dropped when a link is behind. Heard as mchunk events (the window's or a website viewer's own, never the page's). |
| `realtime.lanes` | `/api/v1/realtime/lanes` | local | token | Which players of the seat's room the fast lane reaches now (up), and how (routes: direct link or relay). |
| `referee.get` | `/api/v1/referee/get` | read | — | The referee arcade this one asks for seeds when a page names none, and whose key /r/verified trusts by default: {url, pubkey}, or {} when none is set. |
| `referee.set` | `/api/v1/referee/set` | admin | url | Name the referee arcade (https://...): its key is read from its /r/referee. An empty url clears it. |
| `realtime.status` | `/api/v1/realtime/status` | read | — | Rooms this arcade has players in, and the routes it knows to other arcades' rooms. |

### Ring

| method | route | needs | params | what |
|---|---|---|---|---|
| `ring.send` | `/api/v1/ring/send` | local | page, to, line?, link? | Ring an address from this arcade's player, for the app whose page asks (arcade.realtime.ring): rung, and why not. It reaches them only if they have Notifications on for that app and the caller in their address book. The line (80 characters at most) and link (200) share what one room message carries (about 145 bytes); longer is an error. |
| `ring.waiting` | `/api/v1/ring/waiting` | local | — | What is ringing this arcade's player now; asking also says the app is open, so it can be rung here (the arcade holds its ring room only while this is asked within 20 s and some app has Notifications on). |
| `ring.get` | `/api/v1/ring/get` | local | id, page | The ring an app was opened from (arcade.realtime.ringed), for that app only: from, tag, link, line, at. |

### Inscribe

| method | route | needs | params | what |
|---|---|---|---|---|
| `inscribe.estimate` | `/api/v1/inscribe/estimate` | public | data, encoding?, contenttype?, json? | What inscribing that would cost: transactions, fee, value kept in its data outputs. |

### API keys

| method | route | needs | params | what |
|---|---|---|---|---|
| `apikey.list` | `/api/v1/apikey/list` | local | — | API keys (not their secrets). |
| `apikey.create` | `/api/v1/apikey/create` | local | name, perms, tip_limit? | Make an API key; the secret is shown once. |
| `apikey.revoke` | `/api/v1/apikey/revoke` | local | id | Revoke an API key. |

### RPC

| method | route | needs | params | what |
|---|---|---|---|---|
| `rpc.help` | `/api/v1/rpc/help` | public | method?, prefix? | Every command, its parameters and what it needs. method (one exact command name) or prefix (the start of command names, e.g. tx. or ledger.item) answers only the matching ones. |
| `arc.status` | `/api/v1/arc/status` | public | — | The ARCADE chain run here: height, tip, rules {now, known, protocol, update_needed, outdated_peers}, the pool, storage weights, owners, genesis earners and this arcade's pay (all from the chain). {on: false} when it doesn't run here. |
| `rpc.info` | `/api/v1/rpc/info` | local | — | Where the RPC and the web API are, and the cookie file. |
| `rpc.openapi` | `/api/v1/rpc/openapi` | public | prefix? | The OpenAPI 3.1 document of the web API (also GET /api/v1/openapi.json): every command, its typed parameters and result shape. prefix (the start of command names, e.g. tx. or ledger.item) keeps only those commands' paths; components stay whole. |
| `rpc.guide` | `/api/v1/rpc/guide` | public | — | The short guide for AI agents and scripts (also GET /api/v1/agents.md), as Markdown text. |

### Svc

| method | route | needs | params | what |
|---|---|---|---|---|
| `svc.pay` | `/api/v1/svc/pay` | wallet | to, amount, memo, client_ref? | Pay ARCADE to an address with a memo of up to 80 bytes (a service's credit). |

### Token

| method | route | needs | params | what |
|---|---|---|---|---|
| `token.send` | `/api/v1/token/send` | local | propertyid, units, to, chain?, send?, client_ref? | Send arcade tokens (base units) from this wallet in one transaction, on Pepecoin testnet's ledger or the ARCADE chain's (chain: arc / arctest), installed or borrowed; without send: true, only what it would do. |

### Ledger

| method | route | needs | params | what |
|---|---|---|---|---|
| `ledger.announce` | `/api/v1/ledger/announce` | wallet | client_ref? | Publish this wallet's profile (messaging key under its @tag) on the ARCADE chain. |
| `ledger.tag_commit` | `/api/v1/ledger/tag_commit` | wallet | tag, client_ref? | Commit ahead to claiming a released @tag on the ARCADE chain. |
| `ledger.tag_checkin` | `/api/v1/ledger/tag_checkin` | wallet | client_ref? | Keep this wallet's @tag on the ARCADE chain with a check-in (0.1 ARCADE into the pot). |
| `ledger.repo_push` | `/api/v1/ledger/repo_push` | wallet | name, manifest, base?, force?, client_ref? | Point @owner/name at a git manifest blob (owner or maintainer). From rules v4 the push names base (the manifest it was built on, "" for a new repo; default: the current one) and is refused if the repo moved on, unless force. |
| `ledger.repo_maintainer` | `/api/v1/ledger/repo_maintainer` | wallet | name, tag, add?, client_ref? | Add or remove a maintainer of your repository. |
| `ledger.repo_fork` | `/api/v1/ledger/repo_fork` | wallet | from, name, client_ref? | Fork @owner/name under your @tag. |
| `ledger.item_state` | `/api/v1/ledger/item_state` | wallet | repo, number, state, manifest?, client_ref? | Close, reopen or merge an issue or pull request. |
| `ledger.items` | `/api/v1/ledger/items` | read | repo, number?, state?, kind?, since?, summary?, pending?, cursor?, limit? | A repository's issues and pull requests with their comments and reviews, by number. Filters: state (open, closed, merged), kind (issue, pull), since (a height: items with anything at or after it, and each item's events trimmed to those at or after it); summary: true leaves out the events and keeps the title, plus last_by (who made the newest event). Every item carries last_height (its newest event's height, before since trims). Items, comments, reviews and state changes still waiting in the pool come too, marked pending: true (a pending item has no number yet; pending: false leaves them out). Paged: limit (default 50, at most 500) and cursor (the next of the page before; opaque); next is null on the last page, which also carries the pending items. count: how many on this page. |
| `ledger.book` | `/api/v1/ledger/book` | public | network?, base?, quote? | The market's open orders on the arcade ledger (docs/MARKET.md). |
| `ledger.trades` | `/api/v1/ledger/trades` | public | network?, base?, quote?, before? | The market's recent trades on the arcade ledger. |
| `ledger.pairs` | `/api/v1/ledger/pairs` | public | q? | The market's pairs on the arcade ledger. |

### Arc

| method | route | needs | params | what |
|---|---|---|---|---|
| `arc.blob_publish` | `/api/v1/arc/blob_publish` | wallet | path \| hex, type, class?, gzip?, send?, client_ref? | Put a file on the ARCADE chain as a blob. The fee is per started kB by class from rules version 5 (Temporary 0.000423, Release 0.00282, Permanent 0.003948, Boost 0.000282 ARCADE), 0.01 before. Copies kept: Permanent 3 from rules version 6 (7 before), Release 5, Temporary 3 for 90 days; a Temporary blob can be made Permanent later for the difference (arc.blob_upgrade). Without send, only the quote (blob_fee, fee). |
| `arc.blob_publish_large` | `/api/v1/arc/blob_publish_large` | wallet | path \| hex, type, class?, gzip?, name?, send?, client_ref? | Put a file of up to 100 MB on the ARCADE chain as one large blob: parts of 4 MB over successive blocks, then a manifest naming them and the overall root; its id is the manifest's. Without send, the quote (parts, fee, blocks, seconds). Resumes after a restart without paying twice. arc.blob_publish switches to it above 4 MB. |
| `arc.blob_upgrade` | `/api/v1/arc/blob_upgrade` | wallet | id, send?, client_ref? | Make a Temporary blob Permanent (ARCADE rules version 6): pay the Permanent fee minus the Temporary fee for its bytes (0.003525 ARCADE per started kB), from this wallet; anyone may upgrade any Temporary blob, and it stays its publisher's. A large blob's manifest id upgrades all its parts, paid for their bytes too. Refused for an unknown, Permanent, Release or expired Temporary blob, and before version 6. Without send, only the quote. |
| `arc.block` | `/api/v1/arc/block` | public | height? \| id?, raw? | One block of the ARCADE chain run here: by height (the branch it is on) or by id (any branch it holds): id, prev, height, time, difficulty, writer, score, steps, best, txs (txids), raw (with raw: true, the transactions' hex), bytes, pot_runs. |
| `arc.submit` | `/api/v1/arc/submit` | post | hex, blobs? | A signed ARCADE transaction (hex) into this arcade's pool, from where it goes to the other arcades and into a block; blobs: the hex of blobs it brings. As tx.broadcast for the ARCADE chain. |
| `arc.pool` | `/api/v1/arc/pool` | public | — | The ARCADE transactions waiting in this arcade's pool, in order of arrival (at most 1,000 listed). |
| `arc.catalog` | `/api/v1/arc/catalog` | public | — | The segments the ARCADE chain lists, the entries that wait (and who stands behind each), and this arcade's side of it. |
| `arc.genesis` | `/api/v1/arc/genesis` | public | — | Where the ARCADE chain's genesis list stands here. |
| `arc.segment_root` | `/api/v1/arc/segment_root` | read | chain, first, last | The storage root this arcade makes for heights first..last of a chain it keeps. |
| `arc.replicas` | `/api/v1/arc/replicas` | read | — | The sealed copies (replicas) this arcade holds, and a sealing under way. |
| `arc.sealing` | `/api/v1/arc/sealing` | read | chain | The segments of a chain as this arcade holds them for the ARCADE chain: waiting, queued, sealing, registering, earning; this arcade's weight and the chain's total; why sealing waits. |
| `arc.open` | `/api/v1/arc/open` | read | segment, leaves | Leaves of one of this arcade's replicas, opened (segment: its id; leaves: indexes). |
| `arc.seal` | `/api/v1/arc/seal` | admin | segment? | Seal a confirmed segment (its id; else the first listed one of a chain kept here not sealed yet) under this arcade's key, in the background. |
| `arc.reveal` | `/api/v1/arc/reveal` | admin | segment, first?, count? | A reveal demand by hand: the holders of a listed segment must show these leaves on the ARCADE chain. |
| `arc.serve_check` | `/api/v1/arc/serve_check` | admin | owner, chain, first? \| hashes?, count? | A serving check by hand, no verdict posted: does the arcade `owner` (64 hex) hand out these blocks of chain? |
| `arc.keeper` | `/api/v1/arc/keeper` | admin | on? | This arcade's time keeper: on: true or false turns it on or off (it starts or stops within a block); its state. |
| `arc.archive` | `/api/v1/arc/archive` | admin | on? | Keep every blob's plain copy (an archive node; on: true) or let the classes drop them. |
| `arc.blob_publish_many` | `/api/v1/arc/blob_publish_many` | wallet | files, send?, client_ref? | Several blobs in one ARCADE transaction: files [{hex | path, type, class?, gzip?}] (path: this computer's socket or cookie RPC only); without send, only what it would cost. |

### A

| method | route | needs | params | what |
|---|---|---|---|---|
| `a.profile_set` | `/api/v1/a/profile_set` | wallet | name?, bio?, url?, picture?, addresses?, client_ref? | Publish your profile on the ARCADE chain (A://). |
| `a.follow` | `/api/v1/a/follow` | wallet | tag, client_ref? | Follow an @tag on the ARCADE chain. |
| `a.unfollow` | `/api/v1/a/unfollow` | wallet | tag, client_ref? | Stop following an @tag. |
| `a.contact_save` | `/api/v1/a/contact_save` | wallet | id?, name, tag?, addresses, note?, client_ref? | Save an address book entry (encrypted to your own key). |
| `a.contact_add` | `/api/v1/a/contact_add` | wallet | tag \| address, client_ref? | Add someone to your address book from their published profile. |
| `a.contact_delete` | `/api/v1/a/contact_delete` | wallet | id, client_ref? | Remove an address book entry. |
| `a.wallets_publish` | `/api/v1/a/wallets_publish` | wallet | force?, client_ref? | Publish proofs of this wallet's addresses on every coin and of each imported wallet (each signs with its own key), when they differ from the chain's; done by itself after a profile is published. |
| `a.post` | `/api/v1/a/post` | wallet | text, client_ref? | Post to your followers on the ARCADE chain. |
| `a.react` | `/api/v1/a/react` | wallet | chain, txid, kind, client_ref? | Like, unlike, dislike or undislike any chain's transaction, on the ARCADE chain (A:// op 8). |
| `a.reply` | `/api/v1/a/reply` | wallet | chain, txid, text, client_ref? | Reply to any chain's transaction (1 to 500 bytes), on the ARCADE chain (A:// op 9). |
| `a.tip` | `/api/v1/a/tip` | wallet | chain, txid, amount, to?, memo?, client_ref? | Tip on any chain's transaction: pay amount (sats of ARCADE) to `to` (an ARCADE address; default the transaction's author where one is found) with the tip record (A:// op 10, memo up to 100 bytes) in the same transaction. |

### Code

| method | route | needs | params | what |
|---|---|---|---|---|
| `code.status` | `/api/v1/code/status` | public | — | The releases known here and which of their files are held. |
| `code.add` | `/api/v1/code/add` | local | record, sig, files | A release made here (scripts/code-publish.py), with its files ({name: path} on this computer). |
| `code.export` | `/api/v1/code/export` | local | id, dir | A release's held files written into dir (a folder on this computer) under their names. |

### Swap

| method | route | needs | params | what |
|---|---|---|---|---|
| `swap.list` | `/api/v1/swap/list` | read | — | This arcade's own swaps (as maker or taker) and the step each is at. |
| `swap.take` | `/api/v1/swap/take` | local | node, order, give?, get? | Take an order whole (node: the maker's arcade): the secret is made here and this wallet's leg locked; with the legs shown ({give, get}), only on exactly those terms. |

### Events

| method | route | needs | params | what |
|---|---|---|---|---|
| `events.since` | `/api/v1/events/since` | read | cursor?, kinds?, limit?, wait? | The arcade's event log, oldest first, after cursor (from events.since's next; none: from now, "0": from the oldest kept), of kinds (comma list or array) only; limit (default 100, at most 1,000); wait: seconds (at most 60) to wait for the first new event. {events: [{cursor, at, kind, text}], next, waited}. |

### Dogefeed

| method | route | needs | params | what |
|---|---|---|---|---|
| `dogefeed.post` | `/api/v1/dogefeed/post` | wallet | type?, text?, media?, reply?, quote?, pay?, ref?, tags?, mentions?, send?, client_ref? | Post, comment, quote or edit on D:// from this arcade's own Dogecoin wallet: signed, stamped by this arcade and inscribed as DogeFeed writes them. Without send: the record and its cost. |
| `dogefeed.act` | `/api/v1/dogefeed/act` | wallet | op, target, amount?, send?, client_ref? | Like, branch, follow, tip, reveal, delete or keyrevoke on D:// from this arcade's own Dogecoin wallet (1 DOGE in all for likes, branches and follows; amount in DOGE for tips, 90/10). Without send: what it would send. |

## For agents and scripts

- **Starting and stopping.** The web port opens before the chains. Until the arcade is ready, every request is
  answered at once with `503` and `{"ok": false, "state": "starting", "code": "starting", "retry_after_s": 5}`.
  Once a stop begins, the answer is `503` with `"state": "stopping"`. The socket gives the same answers, and
  `status` carries `phase` (starting, ready, stopping). A send on the ARCADE chain made before that chain can be
  reached here (no arcade that keeps it has answered yet, or its borrowed copy is still being set up), or while an
  installed copy is still opening, gets the same `starting` answer with `retry_after_s`.
- **Error codes.** Every error keeps its `error` text and adds a stable `code`: `unknown_command`, `unknown_route`
  (`/api/v1/<unknown>` is `404`), `not_found` (`404` over the web API), `bad_args`, `not_enough_arcade`,
  `insufficient_funds`, `unknown_chain`, `not_installed`, `forbidden`, `starting`, `stopping`, or `error`. JSON-RPC
  errors carry it in `error.data.code`.
- **One chain's status.** `status chain=arctest` (`/api/v1/status?chain=arctest`) gives:
  - `tip {height, hash}`;
  - `best_known`, the best height linked peers tell of, with `best_known_from` and `best_known_age_s` (null while
    none has told one);
  - `behind`, `sync` (synced, syncing, unknown, not_kept), `mode` and `source` (own or borrowed);
  - for the ARCADE chain, `rules` with `update_needed`.

  `status` alone has `rules.update_needed` too, and the log repeats that warning every 10 minutes while it holds.
  `arc.status` is public.
- **Receipts.** `ledger.item_open`, `ledger.item_comment`, `ledger.send` and `wallet.send` (with `send`) take an
  optional `client_ref`. The same key sent again returns the first answer with `"replayed": true`, and nothing is
  signed twice. Each answer carries a `receipt`. `tx.status chain txid` (or `client_ref`) gives one of these states:
  - `confirmed {height, block, confirmations}`;
  - `accepted_local`: in this arcade's pool; on the ARCADE chain it adds `broadcast {peers_linked, peers_sent,
    peers_holding}`, where holding means peers that named it back as being in their pool;
  - `signed`;
  - `dropped`;
  - `unknown`, with `ever_signed_here`.

  `tx.wait chain txid until timeout` answers once that state is reached.
  `tx.resend chain txid` (or `client_ref`) sends one this arcade signed again, the way it first went out, without
  signing anything new, and answers its `tx.status` with `resent [{txid, ok, how | error}]`; `not_found` when nothing
  signed here matches.
- **Explorer answers** for tx, block, address, summary and mempool carry `chain`, `as_of {height, hash}` (the tip
  they were counted against), `source` and `decimals`. Every other object answer of the Explorer, and
  `status chain=`, carry `source` too:
  - `local`: this arcade's own copy;
  - `borrowed:<node>`: a lender or another arcade, with its node id when known;
  - `copy`: its own copy, still being imported from a shared copy;
  - `none`: not kept here.

  `status chain=` keeps the older own/borrowed word in `source_detail`.

Part 2 (ARCADE Git #25):

- **One surface.** Every command is in one table. The socket, JSON-RPC and `/api/v1` answer the same names, with
  the same arguments, answer shapes and error codes. `rpc.help`, `GET /api/v1/routes` and
  `GET /api/v1/openapi.json` (OpenAPI 3.1, also `rpc.openapi`) list them all, with typed parameters (`schema`:
  name, type, required, default, enum, maximum, one_of) and result shapes. Permissions are unchanged: a `local`
  command over an API key is `403 forbidden`. Only `service.stop` stays socket-only. 32 commands that once
  answered on the socket alone are now on every surface: `arc.block`, `arc.pool`, `arc.submit`, `ledger.book`,
  `swap.list`, `explorer.site` and the rest. A test fails when a command is on one surface but not the others.
- **The guide.** `GET /api/v1/agents.md` (also `rpc.guide`) is a short guide for agents. It covers keys, chains,
  units, error codes, retries with `client_ref`, waiting and paging.
- **Receipts at signing.** Every command that signs takes `client_ref`. This covers ledger.*, a.*, wallet.send,
  borrow.send, token.send, ord.send, msg.send, feed.post, feed.act, follow, tag.claim, arc.blob_publish*, arc.blob_upgrade,
  dogefeed.post, dogefeed.act, inscribe and svc.pay (`sends: true` in rpc.help).
  - The receipt is written the moment a transaction is signed, before any network step. `tx.status` answers
    `signed` even if the call timed out.
  - A retry with the same `client_ref` never signs again. It gets the first answer (`replayed: true`); while the
    first is still running, `in_progress: true` and its txids; after a failure that followed signing, code
    `client_ref_used`.
- **Per-peer broadcast results.** ARCADE pool gossip has a new reply, `txr`. An arcade that fetched transactions
  says which it took and which it refused, with a code. Older arcades don't send it and ignore it. No block rule
  changes.
  - `tx.status` gives `broadcast.peers_acked`, `peers_refused` and `refusals [{peer, code, error}]`. Lenders of a
    borrowed chain are counted too.
  - `broadcast_failed` means refused here, or by every peer that answered while none holds it.
  - Codes: `fee_too_low`, `conflict`, `missing_coin`, `dust`, `not_yet_valid`, `blob_missing`, `pool_full`,
    `rules`, `invalid`, `refused_recently`, `unreachable`.
- **`ledger.items`** includes items, comments and state changes still in the pool (`pending: true`; pending
  items come on the last page). It pages with `limit` and `cursor`/`next`.
  - Every item has `last_height`, its newest event's height. With `since`, each item's `events` keep only those
    at or after it (pending ones too); `comments` still counts them all.
  - With `summary: true` an item also has `last_by`, who made that newest event.
- **No polling.**
  - `explorer.blocks order=asc from=` is a block feed, oldest first (`next_from`).
  - `chains.wait {chain, height, timeout}` waits for a height.
  - `events.since {cursor, kinds?, limit?, wait?}` follows the event log with a cursor.

## Sign-in for websites

A website wired to the arcade can let its users in **with a password, with ARCADE ID and no password, or both**.
Either way the 12 words are the account and stay in the user's browser; the website's server keeps its own sessions.

**Password** (the original arcades' way). Sign-up: the browser seals the words under the password and signs
"ARCADE vault @tag address pubkey sha256" with the @tag's address; the server passes it to `vault.put`. Sign-in: the
server hands the browser `vault.get`'s sealed record, and the password opens it there. Serve `vault.get`'s answer at
`https://<site>/signin/<tag>` too, so other arcades can sign in accounts made on the site.

**ARCADE ID, no password** (built on [Digi-ID](https://www.digi-id.io), created by the DigiByte community). Sign-in:
`id.start` gives a code; the page shows it as a QR code; the wallet posts its answer to the site's callback URL, which
passes it to `id.callback`; the page asks `id.poll` until it is done and gets the address that signed, which the site
links to its user. A new device gets the wallet from one the user already has: `id.link_start` with the new browser's
one-time public key gives a QR code and a 6-digit number; the other device scans it, shows the same number, and sends
the words sealed to that key with `id.link_send`; the new browser collects them with `id.link_poll` and keeps them
locked on the device (a passkey, for one). The arcade only passes the sealed message on; nothing it keeps can open it.

**Signing in to other Digi-ID sites.** The desktop app answers a Digi-ID code itself (`id.answer`), as a phone
wallet such as Coinomi does. A website's users sign in the browser, and the site's server hands the answer to
`id.relay`, which sends it on: a page can't post to another site, but the arcade can. Both make the same key for a
site from the same 12 words.

**Signing in to other Digi-ID sites is a per-site key.** `id.answer` and Digi-ID wallets sign with a key made for that
one site (SLIP-0013): it proves the same person comes back, but it holds no coins and isn't the account's @tag key, so
nothing can be paid or posted with it.

**Account sign-in and payments: ARCADE ID signing.** For a site that acts *as* the account (posts, likes, replies,
tips, payments, as the ARCADE app does), pair it with the app (`id.sign_pair`, an `arcadesign://` code): the app tells
the page the account's key on the chain its @tag is on (`home: {chain, address, pubkey}`, the one its @tag names) beside its Dogecoin
key. Sign-in: the page sends `{type: "message", key: "home", message: <the site's nonce>}` and checks the answer with
`msg.verify {address: home.address, message, signature}` (ARCADE's prefix). Actions: the site's server builds them
unsigned (`psbt.feed_post`, `psbt.feed_act`, `psbt.send` with `home.address` and `home.pubkey`), the page sends
`{type: "psbt", chain: home.chain, psbt, finalize: true}`, the user approves it in the app, and the answer's `txHex`
goes to `tx.broadcast`. The 12 words never leave the app.

**Both.** Offer the password and ARCADE ID side by side. A user who turns the password off: `vault.delete`, signed
by the @tag's address, drops the sealed record, and the account then opens on the site with ARCADE ID or the 12
words only.

## The explorer for apps

An app page reads the same public explorer data as V1.0's own Explorer through the page API:

    GET /r/explorer/<name>?args=<JSON object, URL-encoded>

answers the public command `explorer.<name>` (`/r/explorer/chains` answers `chains.list`) with
`{"ok": true, "result": ...}` or `{"ok": false, "error": ...}`, as `/api/v1` does. The arguments are one JSON
object, exactly as the Explorer sends them (`{"chain": "peptest", "id": "1567902"}`: a height stays text).
No page pass is needed (public data); like the rest of `/r/`, it answers this computer only and app frames
(origin `null`) or this arcade's own pages, never other websites. Only the Explorer's own reads are
answered: `summary`, `blocks`, `block`, `tx`, `address`, `inscriptions`, `inscription`, `inscription_types`,
`tokens`, `token`, `collections`, `collection`, `collection_of`, `held`, `names`, `name`, `maps`, `map`,
`mempool`, `ledger`, `ledger_tokens`, `ledger_token`, `ledger_inscriptions`, `ledger_inscription`, `tag`,
`search`, `msg_rows`, `msg_file`, and `chains`; any other name is 404.

## Repositories for apps

The ARCADE chain's git repositories (docs/ARCADECHAIN.md §15 steps 6, 9 and 10), for an app that shows them as a
code host does (`crates/arcaded/src/arcgit.rs`, `reposvc.rs`). Reads are public data and need no page pass:

    GET /r/git/repos?owner=<tag>            every repository (ledger.repos)
    GET /r/git/repo?name=@owner/name        one, with its maintainers and push history (ledger.repo)
    GET /r/git/items?repo=@owner/name       its issues and pull requests with their texts (ledger.items)
    GET /r/git/blob/<id>                    a blob the chain holds, as stored: a manifest, a pack, its index

The app reads git objects from the packs itself. Writes need the page's pass and the wallet owner's approval:

    POST /r/repo/<op>     push | fork | maintainer | item_open | item_comment | item_state   (202: the request)
    GET  /r/repo/req/<id> where it stands: status, and repo.state working | done | failed with its txids
    GET  /r/repo/me       this wallet's @tag on the ARCADE chain

A request is filed like `/r/send` and shown over the app (Approve or Refuse); it is signed by the wallet's @tag on
ARCADE whatever chain the app is on. A push carries `files` (base64: packs, each followed by its index, then the
manifest last, 3.8 MB at most); the manifest names a new pack by `name` (its checksum) with empty `pack`/`idx`
ids, which the arcade fills in, and every pack it names must be on the chain or in the push. `merge: <number>`
makes the push a pull request's merge (the merge record moves the repository). From ARCADE rules version 4 a push
names its `base`, the manifest it was built on (64 hex, `""` for a new repository; left out, the manifest's `prev`,
else the repository's manifest when the request is made), and `force: true` replaces whatever is there: without force, a push whose base
the repository has moved on from is refused (the request fails with the reason, or the record is invalid if another
push landed first). The work runs in the background: records wait for the wallet's coins, a push for its manifest's
block, then the push record. The socket command is `ledger.repo_push {name, manifest, base?, force?}` (same rules).

**Signing as another @tag** (socket requests): `"as": "@tag"` on `ledger.*`, `a.*`, `arc.blob_publish*` and `arc.blob_upgrade` signs and
pays with the key that holds that tag on ARCADE: one of the wallet's own, or a key imported for the ARCADE chain with
`wallet.import {chain: "arctest", secret: <WIF or words>}` (kept encrypted like the wallet). Without `as`, nothing changes.
`scripts/git-publish.py --as @tag` uses it. A text record (an issue, a comment) is two transactions, its text's blob and
the record, so it waits until the key has a free coin for each.

**Plain `git push` and `git clone`:** `scripts/git-remote-arcade` is a git remote helper (put it on PATH). With the remote
`arcade::@owner/name`, `git clone`, `git fetch` and `git push` work through this computer's arcade socket: a push packs
what the repository's manifest doesn't reach yet, publishes it (any size: a pack over 3.5 MB, one big file, goes as
parts joined by a file manifest blob, `{"arcade": "file", ...}`, which the arcade's git reads join back), then the
manifest and the push record; a push that isn't a fast-forward needs `--force`, and the first push to a name under
your own tag creates it. The record names the manifest the push read first, so (rules version 4) a push that another
one beat to the chain is refused instead of rolling it back: fetch and push again, or force. Settings: `ARCADE_SOCK`/`git config arcade.sock`, `ARCADE_AS`/`arcade.as` (sign as a tag).

**Arcades that borrow the ARCADE chain** (it isn't installed, so no ledger or blobs are kept) answer `/r/git/*` and
`/git/*` by asking an arcade that lends the chain (core RPC method `arcade_git [path, query]` →
`{code, ctype, gzip, b64}`); packs, indexes and blobs are kept 96 MB deep. The lender's word, like borrowed blocks;
git checks every object it clones.

**Arcade V1.0's own code** shows as the read-only repository `@arcade/arcade-v1` (`relrepo.rs`): the newest held
signed release's git bundle, its pack served as is and its index built once beside it, with a tag per release.
`/r/git/repos` lists it, `/r/git/repo` describes it (`release`, and `log` = the releases), it has no issues, and
`git clone http://<arcade>/git/@arcade/arcade-v1` clones it. @arcade is a reserved tag, so nobody can take the name;
a ledger repository of that name would win.

## @tags per chain

Every chain has its own @tags (2026-11-18): @alice on Dogecoin and @alice on ARCADE can be two different people.

- **Full name:** the name and the chain it was claimed on, written `@alice.doge` (the chain's key: `arctest`,
  `peptest`, `pep`, `doge`, `dgb`). Tags are `[a-z0-9_]`, so the dot can't be part of a name. The app shows the ticker:
  `@alice · DOGE`.
- **Claiming:** a tag is free on a chain when nobody holds it on that chain; holders on other chains don't matter.
  Tags can be claimed on ARCADE, Pepecoin testnet, and the mainnet chains arcades index posts and messages on.
- **A plain `@alice`** works wherever a tag is taken (`wallet.send` to, `msg.send` to, `follow`, `profile`, the feed's
  `tag`, `explorer.search`) only when one chain has it, or every chain that has it has it for the same key. Otherwise
  the call fails with "@alice is claimed on N chains by different people: @alice.arctest, @alice.doge; say which", and
  `tag.resolve` lists them for a picker.
- **Paying a tag in any coin:** `wallet.send` to `@alice.doge` on Litecoin pays the address @alice.doge's holder has
  proven on Litecoin (A:// op 7 wallet proofs on ARCADE). On the tag's own chain it pays the holder's address; on a
  chain that derives keys the same way (ARCADE testnet beside Pepecoin testnet) it pays the same key there. With no
  proven address the send is refused, never guessed.
- **Posts and messages** show the tag the author holds on the chain they're written on (`author.full`, `author.label`
  in `feed`). `profile` lists every tag the address's key holds, each with its chain (`tags`).
- **Existing tags** stay valid on the chain they were claimed on. Before, a tag held on ARCADE or Pepecoin testnet
  counted for its key on both, the earliest claim winning; now each chain's holder owns it there.
- **Consensus:** unchanged. The ledger has always kept tags per chain (its tag table is keyed by chain); only arcades'
  own checks treated them as one namespace.
