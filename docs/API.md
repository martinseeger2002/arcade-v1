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
`X-Real-IP`, `CF-Connecting-IP` or `True-Client-IP`, are refused). A reverse proxy or tunnel that
adds none of those (a plain `proxy_pass`, `ssh -L`, `socat`) would make
its visitors look like this computer: run arcaded with `--behind-proxy` there, and nothing over the
web port counts as this computer (programs then use the socket). Answers carry no CORS headers. The
operator can open the HTTP port (`arcaded --http-bind 0.0.0.0`) and serve a website from it
(`--site DIR`): visitors then get the site's pages and inscription content (`/content/<id>`,
`/inscription/<id>`), never the API. A public website calls the API from its own server code.

## JSON-RPC

```sh
curl -s -u "$(cat <home>/.cookie)" -X POST http://127.0.0.1:8420/rpc \
  -d '{"jsonrpc":"2.0","id":1,"method":"msg.send","params":{"to":"@apple","text":"hi"}}'
```

Named parameters only (`params` is an object). Batches work. Errors: `-32700` parse, `-32600` bad
request, `-32601` unknown method, `-32602` bad params, `-32000` the command's own error (message says why).
`rpc.help` lists every method.

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
| `status` | `/api/v1/status` | public | — | Home chain, sync state, tags, profiles, mesh links, node id. |
| `peers` | `/api/v1/peers` | read | — | Arcades linked over the mesh right now. |
| `events` | `/api/v1/events` | read | n?, kind? | Recent activity (newest first), optionally of one kind. |
| `mempool` | `/api/v1/mempool` | public | — | Home chain transactions waiting for a block. |
| `feed` | `/api/v1/feed` | public | order?, channel?, tag?, author?, txid?, limit?, offset? | Public posts (popular, new or friends) with replies, counts and tips. |
| `ledger` | `/api/v1/ledger` | public | — | The arcade ledger's fingerprints next to what other arcades report. |
| `profile` | `/api/v1/profile` | public | who | A published profile (who = @tag or address). |
| `ord` | `/api/v1/ord` | public | id?, chain? | An inscription, or ord index counts. |
| `name` | `/api/v1/name` | public | name?, chain? | A name claim, or counts. |
| `map` | `/api/v1/map` | public | map?, chain? | A map claim, or counts. |
| `token` | `/api/v1/token` | public | tick?, chain? | A token, or how many there are. |
| `tokens` | `/api/v1/tokens` | public | owner, chain? | Token balances of an address. |
| `inscribe` | `/api/v1/inscribe` | wallet | data, encoding?, contenttype?, json? | Inscribe content as this wallet's (arcade INSC; data as text or base64 with encoding: base64). |

### Mesh

| method | route | needs | params | what |
|---|---|---|---|---|
| `mesh.udp` | `/api/v1/mesh/udp` | read | — | UDP hole punching: STUN server and proved addresses, NAT kind, which links run over UDP paths (RTT, packets), introductions. `relay`: the media relay (allocations, this month's use, its limit, the address its sockets are reached at). |
| `mesh.udp_set` | `/api/v1/mesh/udp_set` | admin | on?, lan_dial?, stun_port?, turn?, turn_limit_gb?, drop? | Turn UDP paths and introductions on or off (ARCADE_MESH_UDP=0 overrides), offer LAN addresses to every peer, set the STUN port for the next start (0 = none); drop = a peer id prefix whose path to fail now (falls back to TCP). `turn` turns the media relay on the STUN port on or off, `turn_limit_gb` is its monthly limit (100 by default); both from the next start. |
| `mesh.overview` | `/api/v1/mesh/overview` | public | — | The arcade mesh at a glance: every arcade linked or heard lately, its operator, and per chain what it lends, the chain copies it can make (and why not), the index copies it offers, how far it walks ord, and whether its ledger fingerprint matches ours. |

### Tx

| method | route | needs | params | what |
|---|---|---|---|---|
| `tx.coins` | `/api/v1/tx/coins` | public | addresses, chain? | Spendable coins of up to 500 addresses (confirmed, not spent by a waiting transaction, and unconfirmed outputs), to build a transaction for a browser to sign. Coins carrying an ord inscription, and coins of at most 0.01 that no ord index can vouch for, are listed under `held_back` instead (not in `total`), so a payment built from the list never spends an inscription. Home or installed chain, Addresses index on. On a borrowed chain (not kept here, or kept but still syncing or indexing, the home chain included) the coins come from a lender's address index, each checked against its block and unspent at the lender (`"borrowed": true`). |
| `tx.activity` | `/api/v1/tx/activity` | public | addresses, since?, chain? | What changed for up to 500 addresses: history rows above height since, and waiting transactions paying or spending them; height to pass as since next time. |
| `tx.fee` | `/api/v1/tx/fee` | public | chain? | The fee rate (per 1000 bytes) and dust limit this arcade's wallet builds transactions with on that chain (kept or borrowed). |
| `tx.broadcast` | `/api/v1/tx/broadcast` | post | hex, chain? | Send a transaction signed elsewhere (a website user's browser): every input must spend a coin this arcade knows, outputs no more than inputs; pooled here and sent to the coin's network. On a borrowed chain it goes to the lenders' nodes, which check it; refused by all, it is not sent. |

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
| `id.box_put` | `/api/v1/id/box_put` | public | box, msg | A sealed message for the other end of an ARCADE ID signing pairing (box: 32 hex characters only the two ends know; up to 16 waiting, 60,000 characters each, ten minutes). The arcade only passes it on. |
| `id.box_take` | `/api/v1/id/box_take` | public | box | Everything waiting in a box, oldest first; the box is emptied. |
| `id.sign_pair` | `/api/v1/id/sign_pair` | local | qr, send? | Pair a website with this app so it can ask the app to sign: an `arcadesign://site/<pairbox>?k=<page X25519 key>[&a=<arcade>]` code in. Without send: the site, the 6-digit number to compare and the account and Dogecoin address. With send: the app puts {v: 1, apk, n, c} in the pair box; both ends derive K = sha256("ARCADE/sign\0" + shared + app key + page key), the request box = first 16 bytes of sha256("ARCADE/sign/req\0" + K), the answer box likewise with "res"; {n, c} = AES-256-GCM under K (nonce hex, ciphertext base64). |
| `id.sign_sites` | `/api/v1/id/sign_sites` | local | — | Websites paired with this app. |
| `id.sign_forget` | `/api/v1/id/sign_forget` | local | id | Unpair a website. |
| `id.sign_pending` | `/api/v1/id/sign_pending` | local | — | What paired sites ask this app to sign (taken from their request boxes every 3 s): a message, or a PSBT (each signed input must carry its previous transaction, which must hash to its txid) with the coins it signs, every output, the fee and whether a signed coin carries an inscription (this arcade's ord index, else the mesh's). Requests inside: {id, type: "psbt", psbt, inputToSign?, finalize?} or {id, type: "message", message}. |
| `id.sign_decide` | `/api/v1/id/sign_decide` | local | id, ok | Sign a waiting request (Dogecoin key m/44'/3'/0'/0/0; legacy P2PKH inputs, ALL, ALL\|ANYONECANPAY or SINGLE\|ANYONECANPAY) or refuse; the answer {id, ok, psbtHex, txHex?} or {id, ok, signature} or {id, ok: false, error} goes to the answer box. |
| `id.answer` | `/api/v1/id/answer` | local | code, chain?, send? | Sign in to a Digi-ID site with this arcade's wallet (the active account), as a phone wallet such as Coinomi does: a digiid:// code in. Without send: the site's name, where the answer goes and this wallet's key for that site (SLIP-0013, never a key that holds coins; the same one the web wallet makes from the same words), to show first. With send: signed and sent to the site. chain: dgb (Digi-ID, the default) or another chain this wallet holds (ARCADE ID). |
| `id.relay` | `/api/v1/id/relay` | public | uri, address, signature, chain? | A website's user signed a Digi-ID code in the browser; the site's server passes the answer here and the arcade sends it to the code's site (a page may not post to another site; a server may). Only a well-formed code, signed by that address, once, and only to a public internet address (never this computer or its network; no redirects). |

### Vault

| method | route | needs | params | what |
|---|---|---|---|---|
| `vault.put` | `/api/v1/vault/put` | public | tag, address, pubkey, blob, signature | Password accounts: keep @tag's sealed record here (the 12 words sealed under the password in the browser: {kdf: PBKDF2-SHA512, iterations, salt, nonce, sealed}; nothing here can open it). address = the one @tag names on chain, or for a free @tag a home-chain address: the record is then kept as a reservation (first come) until the @tag is claimed on chain, and a claim by anybody else ends it; signature = its Core signmessage (ARCADE's prefix) over "ARCADE vault @tag address pubkey sha256", sha256 of the record as {kdf, iterations, salt, nonce, sealed} JSON. Replaces an older one. |
| `vault.get` | `/api/v1/vault/get` | public | tag, here? | @tag's sealed record {tag, address, pubkey, blob, from?}: kept here, else (unless here) from the arcade that holds it. The shape of the original arcades' /signin/<tag>: a website serving it there lets other arcades find accounts made on it. |
| `vault.delete` | `/api/v1/vault/delete` | public | tag, signature | Drop @tag's sealed record here ("stop using a password"): signature = @tag's address signing "ARCADE vault delete @tag address sha256" of the record kept. The account then opens on this site with ARCADE ID or its 12 words. |

### Wallet

| method | route | needs | params | what |
|---|---|---|---|---|
| `wallet.create` | `/api/v1/wallet/create` | local | phrase? | Create a wallet (or restore one from 12 words). Returns the new words once. |
| `wallet.phrase` | `/api/v1/wallet/phrase` | local | — | The wallet's 12 words. |
| `wallet.keys` | `/api/v1/wallet/keys` | read | chain? | Your addresses on a chain, their @tags and balances (on SegWit chains each address type's address and balance under kinds; balance counts every type). balance follows the mempool (confirmed coins less those a waiting transaction spends, plus waiting outputs to you); confirmed is the blocks' figure; free the confirmed coins spendable now (not spent by a waiting transaction, mature, no inscription); pending / pending_coins what is arriving; waiting and dropped this wallet's transactions in the mempool and those that left it without a block. |
| `wallet.addr_kind` | `/api/v1/wallet/addr_kind` | wallet | chain, kind? | The address type the Wallet shows on a chain (legacy, p2sh-p2wpkh, p2wpkh, p2tr; remembered per chain) and the types it offers; kind: remember that one. |
| `wallet.send` | `/api/v1/wallet/send` | local | chain?, to, amount, from?, kind?, all_kinds?, send? | Pay amount (satoshis) on a chain (home, installed or borrowed) from this wallet (from: one of its addresses or an imported wallet's; otherwise the own address with the most), signed here; kind: the address type it's sent from, change back to it (default: from's type, else the one the Wallet shows); all_kinds: also spend the wallet's other address types' coins; send: true broadcasts, otherwise it's only built and shown with its fee (and a warning when the address could be meant for another chain). |
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
| `dd.send` | `/api/v1/dd/send` | local | chain?, to, cents, send? | Send DigiDollars (cents, $1 to $100,000 per send) to a DD.../TD... address, with DGB from the same wallet for the fee (0.35 DGB/kB, at least 0.1, at most 0.5 DGB); send: true broadcasts, otherwise it's only built and shown. |

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
| `tag.check` | `/api/v1/tag/check` | public | tag | Whether a @tag is free and valid. |
| `tag.claim` | `/api/v1/tag/claim` | wallet | tag | Claim a @tag on the home chain. |
| `tag.list` | `/api/v1/tag/list` | read | — | Your tag claims and their progress. |

### Faucet

| method | route | needs | params | what |
|---|---|---|---|---|
| `faucet.ask` | `/api/v1/faucet/ask` | wallet | index?, tag? | Ask the mesh for test coins. |
| `faucet.arc` | `/api/v1/faucet/arc` | wallet | address | The user faucet: ask the mesh to top a website user's ARCADE address up to 0.05 ARCADE, in two coins (once per address a day; a website calls it when a user signs in). This arcade's own wallet gets 1 ARCADE once from the node faucet instead. |
| `faucet.arc_status` | `/api/v1/faucet/arc_status` | read | — | The ARCADE top-up asks this arcade knows and how many it paid today; `node_faucet`: node-gift asks, gifts paid today, nodes known paid, and this arcade's own gift. |

### Messages

| method | route | needs | params | what |
|---|---|---|---|---|
| `msg.me` | `/api/v1/msg/me` | read | — | Your messaging key, fingerprint and whether it is published. |
| `msg.stats` | `/api/v1/msg/stats` | public | — | How many message candidates, posts, acts and machine records the home chain holds. |
| `msg.scan` | `/api/v1/msg/scan` | message | — | Look for new messages now (it also runs every 15 seconds). |
| `msg.who` | `/api/v1/msg/who` | public | name | Who a @tag or address is, and their published messaging key. |
| `msg.conversations` | `/api/v1/msg/conversations` | read | requests? | Conversations, newest first (requests: people not in your contacts). |
| `msg.thread` | `/api/v1/msg/thread` | read | peer | Every message with one person (their messaging key). |
| `msg.read` | `/api/v1/msg/read` | message | peer | Mark a conversation read. |
| `msg.unread` | `/api/v1/msg/unread` | read | — | Unread counts: messages, requests, group lines. |
| `msg.file` | `/api/v1/msg/file` | read | id | A message's file (base64). |
| `msg.send` | `/api/v1/msg/send` | message | to, text?, file? | Send a private message to a @tag, address or key; file = {name, type, data (base64)}. |
| `msg.delete` | `/api/v1/msg/delete` | message | id | Stop showing a message here (the chain keeps it). |
| `msg.announce` | `/api/v1/msg/announce` | wallet | — | Publish your messaging key now if it isn't on chain. |
| `profile.publish_addresses` | `/api/v1/profile/publish_addresses` | wallet | send? | List this wallet's own addresses (Pepecoin mainnet, Dogecoin, each other coin installed here) on your @tag's profile, keeping the rest of it; no private key is pasted. send: true broadcasts, otherwise it's only built and shown with its fee and total cost. |

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
| `feed.post` | `/api/v1/feed/post` | post | text?, channel?, file? | Post publicly (needs a @tag). |
| `feed.act` | `/api/v1/feed/act` | post | kind, target, text?, amount? | Like, unlike, dislike, undislike, reply, share, edit, delete; tip (needs tip; amount in satoshis). |
| `feed.file` | `/api/v1/feed/file` | public | txid | A post's file (base64). |

### Ord

| method | route | needs | params | what |
|---|---|---|---|---|
| `ord.chunks` | `/api/v1/ord/chunks` | public | from?, chain? | Ord fingerprint chunks (as other arcades compare them). |

### Explorer

| method | route | needs | params | what |
|---|---|---|---|---|
| `explorer.summary` | `/api/v1/explorer/summary` | public | chain? | Chain summary for the explorer's front page. |
| `explorer.earn` | `/api/v1/explorer/earn` | public | chain? | This arcade's ARCADE pay, read only (docs/EARN.md): its turn, next payout, accrued pay, pay and draw weight, why pay waits, and its sealed replicas waiting to register; with a chain, that coin's Earn ARCADE progress too. App pages read it at `/r/explorer/earn`. |
| `explorer.blocks` | `/api/v1/explorer/blocks` | public | chain?, offset?, limit? | Recent blocks. |
| `explorer.block` | `/api/v1/explorer/block` | public | chain?, id | One block (height or hash). |
| `explorer.tx` | `/api/v1/explorer/tx` | public | chain?, txid | One transaction. |
| `explorer.address` | `/api/v1/explorer/address` | public | chain?, address | An address: balance, history, inscriptions, tokens. pending: the balance with the mempool taken in, what arrives and leaves, and the waiting transactions (when the mempool touches the address). |
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

### Chains

| method | route | needs | params | what |
|---|---|---|---|---|
| `chains.peers` | `/api/v1/chains/peers` | read | key | A chain's P2P address book: nodes known, working in the last day, resting after failures; the working ones and the next to try. |
| `chains.list` | `/api/v1/chains/list` | public | — | Every chain V1.0 knows, installed or not, with sizes reported by arcades. |
| `chains.status` | `/api/v1/chains/status` | public | key | One chain's staged progress. |
| `chains.offers` | `/api/v1/chains/offers` | public | — | Chains other arcades offer over the mesh. |
| `chains.sizes` | `/api/v1/chains/sizes` | admin | — | Measure this arcade's disk use per chain now. |
| `chains.add` | `/api/v1/chains/add` | admin | key, mode?, with? | Install a chain (mode full, shared or chain; with = indexes). |
| `chains.pause` | `/api/v1/chains/pause` | admin | key | Pause a chain. |
| `chains.resume` | `/api/v1/chains/resume` | admin | key | Resume a chain. |
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

### Share

| method | route | needs | params | what |
|---|---|---|---|---|
| `share.held` | `/api/v1/share/held` | public | — | V1.0 copies of chain parts held here (blocks, coins, ord, tokens, names, maps, ledger, profiles, messages): id, height, rows, size; and those being made. |
| `share.offers` | `/api/v1/share/offers` | public | key | V1.0 copies other arcades hold for a chain, per part: id, height, how many made it themselves and how many hold it. |
| `share.fetch` | `/api/v1/share/fetch` | admin | key, fam, id? | Fetch a part's copy from every arcade holding it at once: the newest one 2 or more arcades made themselves (or the id given). Pieces are checked against the manifest; the copy is then held and served here too. |
| `share.install` | `/api/v1/share/install` | admin | key | Install an added chain (or the home chain) from V1.0 copies: its blocks and coins made by 2 or more arcades at the same checkpoint, fetched from every holder and taken in as they come (no replay); the coins are trusted until re-checked. |
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
| `borrow.send` | `/api/v1/borrow/send` | local | chain, to, amount, from?, kind?, all_kinds?, send? | Pay amount (satoshis) on a borrowed chain, signed here (kind, all_kinds: as wallet.send); send: true broadcasts to every lender, otherwise only built and shown. |

### Mining

| method | route | needs | params | what |
|---|---|---|---|---|
| `mine.start` | `/api/v1/mine/start` | admin | threads? | Mine the home chain. |
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
| `apps.launch` | `/api/v1/apps/launch` | read | chain, txid | What an app window opens: the sandboxed page URL and the card; `caution` when the version that opens is one arcade's word. |

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
| `realtime.media` | `/api/v1/realtime/media` | local | token, to, b64 | A piece of the app window's encoded camera, microphone or screen (up to 60 KB) for some players of the room, over the direct link to their arcade, else through at most two arcades on the room's route that have their media relay on; dropped when a link is behind. Heard as mchunk events (the window's own, never the page's). |
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
| `rpc.help` | `/api/v1/rpc/help` | public | — | Every command, its parameters and what it needs. |
| `rpc.info` | `/api/v1/rpc/info` | local | — | Where the RPC and the web API are, and the cookie file. |

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
`search`, and `chains`; any other name is 404.

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

**Signing as another @tag** (socket requests): `"as": "@tag"` on `ledger.*`, `a.*` and `arc.blob_publish*` signs and
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
