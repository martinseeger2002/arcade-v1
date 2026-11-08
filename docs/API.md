# Arcade V1.0 API

Everything the desktop app does goes through one set of commands. Programs reach them three ways:

| Way | Who | Address | Auth | What |
|---|---|---|---|---|
| Local socket | the app, the `arcade` CLI | `<home>/arcaded.sock` | file permissions | every command; one JSON request per line: `{"cmd": "msg.me"}` |
| JSON-RPC 2.0 | sidecar apps on this computer | `POST http://127.0.0.1:8420/rpc` | Basic auth with the cookie file `<home>/.cookie` (`__cookie__:<secret>`, new each start) | every command |
| Web API | websites (their server code) and other programs | `http://127.0.0.1:8420/api/v1/<group>/<name>` | none for **public**; otherwise `Authorization: Bearer ak_...` (keys made in the app's API tab) | commands allowed by the key |

**Only this computer.** `/rpc` and `/api/v1` answer only requests from this computer that didn't come
through a proxy (requests carrying `X-Forwarded-For`, `Forwarded` or `X-Real-IP` are refused). The
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

### Wallet

| method | route | needs | params | what |
|---|---|---|---|---|
| `wallet.create` | `/api/v1/wallet/create` | local | phrase? | Create a wallet (or restore one from 12 words). Returns the new words once. |
| `wallet.phrase` | `/api/v1/wallet/phrase` | local | — | The wallet's 12 words. |
| `wallet.keys` | `/api/v1/wallet/keys` | read | chain? | Your addresses on a chain, their @tags and balances. |
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
| `explorer.blocks` | `/api/v1/explorer/blocks` | public | chain?, offset?, limit? | Recent blocks. |
| `explorer.block` | `/api/v1/explorer/block` | public | chain?, id | One block (height or hash). |
| `explorer.tx` | `/api/v1/explorer/tx` | public | chain?, txid | One transaction. |
| `explorer.address` | `/api/v1/explorer/address` | public | chain?, address | An address: balance, history, inscriptions, tokens. |
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

### Chains

| method | route | needs | params | what |
|---|---|---|---|---|
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

### Mining

| method | route | needs | params | what |
|---|---|---|---|---|
| `mine.start` | `/api/v1/mine/start` | admin | threads? | Mine the home chain. |
| `mine.stop` | `/api/v1/mine/stop` | admin | — | Stop mining. |
| `mine.status` | `/api/v1/mine/status` | read | — | Mining state and hash rate. |

### Apps

| method | route | needs | params | what |
|---|---|---|---|---|
| `apps.store` | `/api/v1/apps/store` | public | sort?, q?, flagged? | The App Store: app cards from every chain with the arcade ledger (popular or new; starred first). |
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
| `apps.launch` | `/api/v1/apps/launch` | read | chain, txid | What an app window opens: the sandboxed page URL and the card. |

### Realtime

| method | route | needs | params | what |
|---|---|---|---|---|
| `realtime.join` | `/api/v1/realtime/join` | local | page, room, game? | Join a room over the mesh as this arcade's player (game defaults to the page): a token, me and the members. |
| `realtime.send` | `/api/v1/realtime/send` | local | token, data?, b64? | Send to everyone in the room: text or base64 bytes, at most 512 bytes, about 5 a second. |
| `realtime.leave` | `/api/v1/realtime/leave` | local | token | Leave the room. |
| `realtime.poll` | `/api/v1/realtime/poll` | local | token, wait? | Room events for this seat (join, leave, message), waiting up to wait ms (15000 at most); a seat not polled for 30 s is let go. |
| `realtime.status` | `/api/v1/realtime/status` | read | — | Rooms this arcade has players in, and the routes it knows to other arcades' rooms. |

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
