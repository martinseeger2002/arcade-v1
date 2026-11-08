# The app-facing API (old arcade, c69f944e0) — what V1.0 must match

From the Arcade v1.0 session's read-only survey of ~/.dogecoinarcade/src/arcade, 2026-11-08, sent in
parts (A: frame model, storage, realtime, app/ring; B: access, contacts, groups, llm, node, swap, owner,
state, escrow, every /r/* route; C: viewer ticket, postMessage protocol, the wallet gate). Kept close
to the original text. T = web/templates, PD = T/_page_doors.js.html.

Headline facts (checked by that session):
- Viewer ticket = secrets.token_urlsafe(18) (web/viewer_tickets.py:37), a random bearer token (no HMAC), in
  viewer-tickets.sqlite with a 20000-entry cache, 12 h sliding life, bound to (address, network, page txid).
- Saves: the operator's viewer uses pagestore.py (<home>/pagedata.sqlite, keyed by page txid; key 256, value
  64 KiB, total 1 MiB, 1000 keys). Account/signed-out viewers keep saves in browser localStorage
  "arcade.pagestore.<address|anon>.<txid>". Saves do NOT sync across devices and are not on chain (the docs say
  otherwise; V1.0 may make the promise true).
- Rooms: "<game>/<room>"; game defaults to the page txid; any page may pass any game value (no family check).
- Developer guides (public): https://pepecoinarcade.com/docs/games.md, /docs/inscription-api.md,
  /docs/multiplayer.md, /docs/game-state.md, /docs/escrow.md, /docs/prize-pools.md, /docs/referee.md,
  /docs/llm.md, /docs/swaps.md.

## A0. Frame model
- Layers: optional shell (/home, /desktop) → viewer page (/inscriptions/<txid>/view or /full, includes PD) →
  sandboxed frame `<iframe class="inscription-frame" sandbox="allow-scripts allow-pointer-lock">`,
  src "{pages}/content/<txid>[?v=<ticket>]".
- /content: CSP "sandbox allow-scripts allow-pointer-lock; ... connect-src 'self'", Referrer-Policy unsafe-url.
  Opaque origin: no localStorage, no cookies.
- Shims are plain scripts: GET /r/storage.js, /r/app.js, /r/realtime.js; application/javascript, ACAO *,
  Cache-Control public max-age=3600; the template file verbatim.
- Page→viewer: postMessage to window.parent, targetOrigin '*'. Request {arcade: <door>, seq: <int>, op, ...};
  reply {arcade, seq, ok: bool, error?, ...}.
- Viewer dispatcher (PD:2704-2822): drops a message unless e.source is the frame's contentWindow and
  doors[m.arcade] exists and seq is a number. doors = {access:'viewer', contacts:'viewer', groups:'viewer',
  storage:'/storage/', node:'/node/', swap:'/swap/', owner:'/owner/', realtime:'/realtime/', escrow:'/r/escrow/',
  state:'/r/state/', llm:'viewer', app:'viewer'}. answer(body) sets arcade and seq and posts to e.source with '*'.
  The page's identity `here` = the framed txid from the server, never from the message.
  viewer = "wallet" (operator's machine) | "account" | "nobody".
- Tickets don't authenticate storage/realtime/app (those go by postMessage); they serve the page's own direct
  fetches to /r/* (e.g. /r/wallet): ?v= or the Referer's query; must match the network, and the Referer path
  /content/<txid> must match the ticket's txid.

## A1. arcade.storage (/r/storage.js)
JS (T/storage.js:20-90): window.arcade.storage; module cache `items`.
- ready: Promise = ask({op:'load'}), resolves to storage; sets cache from m.items.
- getItem(key) → string|null (sync, cache; key String()-ed).
- setItem(key, value) → Promise<storage>; cache written first; on reject old value restored (or key deleted).
  {op:'set', key, value}.
- removeItem(key) → Promise; cache first, no rollback. {op:'remove', key}.
- clear() → Promise; cache emptied first, no rollback. {op:'clear'}.
- history(key?) → Promise<object> m.history || {}; {op:'history', key: key===undefined ? '' : String(key)};
  shape {key: [entry,...]} newest first.
- key(i) → Object.keys(items)[i] || null; length getter; all() → shallow copy.
- No events. Each ask rejects after 5000 ms: Error('no wallet is listening: open this page in a DogecoinArcade
  viewer'). Listener doesn't check e.source; matches m.arcade==='storage' + pending seq. {ok:false, error} →
  reject(new Error(error || 'refused')).
Viewer: operator → GET /storage/<here> (load), POST /storage/<here> JSON {op,key,value,csrf_token} (others);
reply passes items/history through; failure {ok:false, error: j.error||j.detail||'refused'}, network
'the wallet did not answer'. Others → browser localStorage (see below).
Server: GET /storage/{txid} → {"ok":true,"items":{...}} (keys ordered; 404 "no such inscription" unless a 64-hex
inscription). POST ops set/remove/clear/history; other op → "op must be set, remove, clear or history"; errors
400 {"ok":false,"error"}.
pagestore.py: item(inscription, key, value, updated) + item_history(inscription, key, value, updated, replaced).
Keyed by page txid only (no address). Limits: key 1..256 ("a key is 1 to 256 characters"), value ≤ 65536
("a value is at most 65,536 characters"), ≤ 1000 keys ("a page may keep at most 1000 keys", new keys only),
total ≤ 1,048,576 chars ("a page may keep at most 1,048,576 characters in all"), history 10 per key (old value
filed when it changes; whole history trimmed to the total, newest kept). history → {key: [{value, updated,
replaced}]} newest first.
Browser store (account/nobody): localStorage "arcade.pagestore.<address|anon>.<txid>" (JSON {key:value}),
history "arcade.pagehistory.<address|anon>.<txid>" ({key:[{value, replaced}]}); same limits and texts; a failed
write "this browser would not hold that for the page: <e.name>".

## A2. arcade.realtime (/r/realtime.js)
JS (T/realtime.js:143-519): window.arcade.realtime = {join, area, ring, ringed, MAX_BYTES: 512}; idempotent.
ask(): arcade 'realtime', seq; default wait 20000 ms → 'no viewer is listening: open this page in a
DogecoinArcade viewer'; reply {pending:'unlock'} extends to 600000 ms ('wallet-locked: unlock your wallet in the
viewer to play online'). Pushed events {arcade:'realtime', event:{type, room, member?, ...}} routed by room.
- join(name, options) → Room (never rejects; failure → Room{online:false, why}). Message {op:'join', room, game:
  options.game||'', fast: !!options.fast, waits: true, link?, max?}. options.cap: if full (members excluding me ≥
  cap) try name, name#2 … #50. Online rooms cached by name.
- Room: name, online, me, why, members(), fast (when said.fast), media (when said.fast && said.media).
  on(type, fn): message, join, leave, closed, rejoined, reconnecting (others throw).
  send(value) (string as is, else JSON) → {op:'send', room, data}; rejects when offline. leave() → {op:'leave', room}.
  Events: join (members[p.id]=p), leave, closed (online=false), me (new id), rejoined (me, media|null),
  reconnecting, fast (JSON-parsed if string), media*, peer, message (JSON.parse(data); if no string data but b64,
  data = the b64 string).
- Player p = {id, node, address, tag, guest}.
- Fast (WebRTC lane): on('message'|'peer'), peers(); send(value, {reliable, to}) → {arcade:'realtime', seq:0,
  op:'fast', room, data, reliable, to?} (never answered); routes() → {op:'fastroutes'} → {id: 'direct'|'relay'|
  'cloudflare'|'connecting'|'none'}; link(ids|null, {max}) → {op:'fastlink'}.
- Media: {op:'media', room, do: start|stop|screen|share|frames|levels|spatial|hear|volume|place|mute|shown, ...}
  (see the original for fields); errors 'off', 'denied', 'no screen', 'cancelled', 'reconnecting', 'not direct',
  'media needs a room joined with {fast: true}'.
- area(base, {game, cell=64, view=cell/2, x, y}): cell rooms "<base>@<cx>,<cy>"; join cells in view, leave as you
  move; on/move/send (own cell)/members/leave.
- ring(address, {line ≤ 80, link ≤ 200}) → {op:'ring', room:'', to, line, link} (wait 60000) → {ok, rung, why};
  ringed() → {op:'ringed', room:''} → said.ring || null.
Viewer door: join (game = m.game || here; no family check; waits → {pending:'unlock'} before unlock) → {ok:true,
online:true, me, members, fast, media, screen} or {ok:true, online:false, why}; send → POST /realtime/send {token,
data} (rejoin on "join again"/"join the room first"); leave; other op 'realtime ops are join, send, media and
leave'. Hidden traffic: data starting "\u001farcade-rtc:" (WebRTC signalling) or "\u001farcade-trade:".
HTTP (web/realtime.py): CORS on /realtime/*; GET /realtime/hello → {ok, online, who:{kind, address, tag}, cert:
{network, node, expires}, sign?}; POST /realtime/join {game, room, guest?, sig?, expires?, address?} — game
^[A-Za-z0-9._:-]{1,64}$, room 1..60 chars, mesh room "<game>/<room>"; operator signed by the node wallet; account
cred {a, x, s}; guests "guest-"+8 hex → {ok, token, online, me, members}; POST /realtime/send {token, data|b64} ≤ 512
bytes, ~5/s ("too fast" 429); POST /realtime/leave; GET /realtime/stream?token= (SSE: "retry: 2000", data frames,
"event: ping" after 4 s quiet, "event: closed"); hub/attach, hub/detach, GET /realtime/hub?id= (SSE {k, ev});
GET /realtime/nodes; GET /realtime/ice?app=<txid> (STUN/TURN).
Cert words: "arcade-realtime\n<network>\n<node_id>\n<address>\n<expires>"; expires = 23 h past the next hour.
Mesh side: one member per (room, member) per node, seats expire ~30-38 s without a stream; vouching cred.a ==
member, now < x ≤ now+86400+3600, Core verifymessage over the cert words; guests ^guest-[0-9a-f]{8}$. Limits: room
≤ 128 chars, member ≤ 64, payload 512 bytes, 256 members per room per node, 2048 rooms per node.
Mesh wire for rooms (mesh/link.py, node.py): links sealed XChaCha20-Poly1305 (nonce = per-direction counter,
24-byte BE). Plain body: u16 BE header length + JSON header + blob, or binary pub "FF FF 01 | origin 32 | sig 64 |
q u64 | hops u8 | kind u8 (msg 0, join 1, leave 2, here 3) | room u8+bytes | member u8+bytes | cred u16+canonical
JSON | d u16 count (0xFFFF absent) + 4 bytes each | payload".
pub {"t":"pub","r":room,"o":origin node hex,"q":µs since epoch (strictly increasing),"m":member ("-" for here),
"k":"msg"|"join"|"leave"|"here","hops","c"?,"d"?:[8-hex short ids],"sig"}; sig = Ed25519 over canonical JSON
(sorted, "," ":") of the header without sig, hops, d, then the payload. Checks: hops < 16, kind, payload ≤ 512
(here ≤ 60 KB), dedup "o:q", |q − now| ≤ 60 s, signature; forwarders rate-limit (origin, member) 10/s burst 20.
here payload: [[member, cred|null], ...] (full) or [[member], ...]; every 8 s per room, full every 6th beat or for a
new node; remote members dropped after 24 s without one.
sub {"t":"sub","o","q","rooms":[16-hex blake2b-8(room)],"hops","res"?,"sig"}: on change and every 30 s; floods;
first arrival sets the route back; 4/s burst 10 per origin; routes expire after 105 s.
Limits: payload 512 B, 5/s burst 10 per member, 256 members per room per node, 2048 rooms per node, 16 hops,
credential ≤ 400 B.

V1.0 status (crates/arcaded/src/realtime.rs): rooms over the mesh as above: pub (JSON and binary), sub routes,
here beats, certs (Pepecoin signmessage over the cert words, "arcade-main"), guests, dedup, hops, rate limits.
The app window's door joins, sends, leaves and long-polls realtime.poll for events. Checked: three local arcades
see each other's joins, messages (~15 ms) and leaves; live sub frames from original arcades verify. Not yet: the
fast lane (WebRTC), media, ring/ringed, area's moving cells, and /realtime/* for web visitors.

## A3. arcade.app (/r/app.js) and ringing
- window.arcade.app = {on(fn), off(fn), state() → Promise}; state object {state: "open"|"minimized"|"restored"|
  "closing", timeout: seconds|null, closesAt: ms|null, inShell}; initial {state:'open', timeout:null,
  closesAt:null, inShell:false}.
- Push from the viewer {arcade:'app', event:true, state, timeout, closesAt, inShell}: strip arcade/seq/event/ok,
  call every fn, dispatch CustomEvent('arcade:app', {detail}).
- state(): {arcade:'app', seq, op:'state'}, 15 s; no parent → 'no viewer: open this page in the arcade'; timeout →
  'no viewer: nothing answered; open this page in the arcade'. Viewer: op 'state' → {ok:true, ...appNow}; other →
  {ok:false, error:'app: op is state'}.
- Minimized timeout (card minimized_timeout, default 900 s, 0 none) is enforced by the shell only: minimized → tell
  'minimized' (closesAt = now + timeout), timer → close; show → 'restored'; close → tell 'closing', remove after
  ~700-800 ms (the app's save window). A plain tab only ever sees 'open'.
- Ringing: realtime.ring(address, {line ≤ 80, link ≤ 200}) → POST /account/ring {to, app: here, line, link, sig,
  expires} (cert proof); rules: the callee switched the app on and allows the caller (address book); RING_GAP 15 s,
  30/hour per pair; over the mesh via room "arcade!ring/<address>"; delivered as a Web Push / open-tab prompt that
  opens the app with ?ring=<id>; the app calls realtime.ringed() → {from, tag, link, line, at} if the ring is for it.

### Part A quirks (observed in the original)
Storage isn't synced (per browser for accounts); keyed by page txid only (a new version starts empty; the operator's
viewers share one shelf); history shapes differ; storage.js times out at 5 s; removeItem/clear don't roll back;
listeners don't check e.source; realtime `game` isn't checked against the card's family; 'closed' is never sent;
b64 messages reach pages undecoded; hub attach/detach skip CSRF; queue policies differ; rate limit shared per
(room, member); seats outlive the 15 s stream timeout; rings answer rung:true early; the minimized timeout is
client-side only; inShell false under /desktop; the service worker reuses the first window; ringed() txid mismatch
between card and page; constants duplicated by hand.

# Part B: the page shims and /r/ routes

## B0. Conventions
- Every /r/<name>.js: GET, the template file unchanged, application/javascript, ACAO *, Cache-Control public
  max-age=3600 (groups, contacts, llm, access, app, storage, node, swap, owner, state, escrow, realtime).
- postMessage: frame → parent {arcade: verb, seq: ++seq (per shim), op, ...}; parent → frame {arcade, seq, ok:true,
  ...} or {ok:false, error}. Shims resolve with the whole message (arcade and seq included) unless noted; ok:false
  → reject(new Error(m.error || 'refused')). Pushes: swap {arcade:'swap', event:{type:'trade',...}}, llm {arcade:'llm',
  seq, part:true, delta}, app {arcade:'app', event:true, ...}.
- Viewer kinds: 'wallet' (operator's machine; parent POSTs /node/<txid>, /swap/<txid>, /owner/<txid>,
  /storage/<txid>), 'account' (keys in the tab), else guest.
- Direct /r/ fetches identify the viewer by the ticket (?v= or in the Referer); Referer path /content/<txid>[iN] must
  equal the ticket's txid; the ticket's network must be the token chain's.
- Inscription row as pages see it (contentlib.describe): {id, number, creator, owner, creatortag|null, ownertag|null,
  block, contenttype, length, sha256, transactions, held:bool, json:<parsed object or null>, rawjson:<string>,
  collection, edition}. Token holding: {propertyid, name, balance (display string), units:int, divisible:bool,
  issuer}. Collection: {creator, name, count, firstnumber, lastnumber, firstedition, lastedition, cover, covertype,
  supply}. Missing → 404 {"error": text} no-store.

## B1. /r/access.js → arcade.access (strip() drops arcade/seq)
- status() → {op:'status'}, 15 s → {ok, wallet:bool, signedIn:bool, node:<host>, game:bool, name}.
- wallet() → {op:'wallet'}, 300 s → {ok, wallet:bool, status:'allowed'|'already'|'declined'}; guest: "sign in first:
  wallet access is for a player who is signed in".
- nodes() → {op:'nodes'}, 30 s → array [{base, host, ms|null, here, arcade, affiliate, name, by}] (here first, then
  arcade, answered, by ms; ≤ 24).
- move(base) → {op:'move', to}, 300 s → {ok, status:'moving'|'declined'|'here'}; only an https affiliate that
  answered; navigates to <base>/play/<card>?from=<host>.
No parent → "no viewer: open this page in the arcade"; timeout → "no viewer: nothing answered; open this page in the arcade".

## B2. /r/contacts.js → arcade.contacts
- list() → {op:'list'}, 120 s, never rejects → [{address, tag}] (empty may carry .why 'off'|'guest'|'operator'|'no'|
  'viewer').
- add(who) (string or {address, name?}) → {op:'add', address, name?}, 300 s → {ok, status:'added'|'already'};
  remove(address) → {op:'remove', address} → {ok, status:'removed'|'absent'}.
Rules: an app with a card follows its Contacts switch ('off'); no card asks once per visit; address must be an @tag
/^@?[A-Za-z0-9_]{1,24}$/ or an address /^[A-Za-z0-9]{25,90}$/ ("not an address: give an address or an @tag"); name
text ≤ 60; one question at a time ("busy: ..."); only claimed tags ("no name: ..."); refusal "declined".

## B3. /r/groups.js → arcade.groups (timeout 120 s "no viewer is listening, or the wallet is still locked")
- list() → {me, groups:[{id, name, room, members:[{tag, address}]}]}; read(id, {after, limit}) → lines oldest first
  [{txid, when, height, mine, from, text}] (after 0, limit 200); send(id, text) → {txid, fee}; leave(id) (600 s) →
  {left: root id}; seal(id, text) → sealed (for a realtime room); open(id, sealed) → text|null; people() →
  [{address, tag}]; add(id, members) → group; create(name, members) → group; save(name, bytes) → {saved}.
- Account only (operator: "operator: group chats are an account's"; guest "sign in first").
- Needs a card with Group chats on ("no card: only an app with a card can open group chats"; "off: turn on Group
  chats on this app's card"); send: not blank ("nothing to send"), ≤ 2000 chars ("too long: at most 2000
  characters"), ≥ 400 ms apart ("busy: wait a moment"); create: name ≤ 60, ≤ 50 members; add ≤ 50; one question at a
  time; refusal "declined"; unknown group "not a group of yours".

## B4. /r/llm.js → arcade.llm (no timeout; parts {part:true, delta} → onToken)
- offers() → [{seller, here, free, name, tag, models:[{id}], prices:[{leg, label, in, out}], max_out}];
  chat(req) (OpenAI chat fields + seller, leg) → {content, reasoning, usage:{tokens_in, tokens_out}, cost, left,
  seller, leg}; image(req {prompt, size, image, seller, leg, model}) → {png, url, seed, cost, left, seller, leg}.
- Account only ("sign in first: the model is paid from your credit"); a per-page yes kept in localStorage
  'arcade.llm.ok:<txid>'; sellers on other arcades need a signed grant; credits per buyer per model.

## B5. /r/node.js → arcade.node (60 s, "no wallet is listening: open this page in a DogecoinArcade viewer")
- identity() → {ok, pubkey (64 hex X25519), contactcode, network, maxbytes}; send(to, body) → {ok, txid, to,
  fromaddress, size, fee, total}; sent() → [{to, txid, created}] newest first ≤ 50; replies({after, limit}) → oldest
  first [{id, txid, block, blocktime, frompubkey, fromaddress, body, json, read, protocol, apihash, compatible}];
  listen(handler, {after, every=10000}) polls replies (no after: from now) → stop().
- Server (operator): POST /node/<txid> {op, ...}; Pepecoin test only ("node-to-node messages are Pepecoin test
  only"); messages are API (type 6) sealed with the node's identity; replies only from peers this page wrote to,
  newer than its first letter to each; limit ≤ 100; ≤ 30 sends per page per hour. Account: signed and paid in the
  tab with the account key. Guest: "sign in first — ...".

## B6. /r/swap.js → arcade.swap (60 s; push {arcade:'swap', event:{type:'trade', id, status, with, give, get, role,
why, txid}})
- Shop (the page is a shop inscription, JSON {"shop":{"node", "listings":[{give, take}]}}, 1..200 listings): shop() →
  {ok, shop, node, can_buy, seller, network, mine, open, listings:[{n, give, take, text, available}], height, from,
  ready}; leg {kind:'inscription'|'token'|'coins'|'random'|'unknown', ...}; offer(n), awaitOffer(txid), accept(offer),
  status(request), awaitDecision, awaitSwap, buy(n) (the whole flow), describe(leg).
- Player trades (need a realtime room): trade({with, give, get}) → {id}; answer(id, accept, why); cancel(id); items()
  → {address, inscriptions, balances}; onTrade(fn); statuses proposed, incoming, accepted, signed, broadcast,
  settled, declined, cancelled, failed.
- Pepecoin test only ("shops are Pepecoin test only"); the buyer approves in the wallet; the shop's node
  countersigns only a transaction matching its own offer; the seller sells only while creator and holder.

## B7. /r/owner.js → arcade.owner (60 s)
- identity() → {ok, owner:bool, creator, holder, network}; send({kind:'coins'|'token'|'inscription', to, amount,
  propertyid, inscription, from, label, note}) → {ok, txid, request, what, fee}.
- Without asking only on Pepecoin test and only for a page this wallet created and still holds ("only a page this
  wallet created and still holds sends without asking; this one has to ask (arcade.send)"); mainnet asks.
(Rest of Part B and Part C: appended as they arrive.)
