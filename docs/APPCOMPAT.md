# Deployed apps on V1.0 — PLAN

Status: **build order steps 1 to 3 are built on branch `claude/app-compat`, not committed; steps 4 to 7
are not.** See "Built so far". Written 2026-11-10 on the GX10 from a read-only audit. Asked for by Martin:
every app deployed on the original arcade must run 100% on V1.0.

## How it was checked

- The app list is one live original arcade's ranking (the list its Apps tab shows; removed and hidden
  cards are already left out): **26 app cards**.
- For each card, every inscription its code loads was followed, including inside compressed parts:
  **578 inscriptions, 20 MB**. That code was searched for shims, doors, door ops, `/r/` fetches and the
  fields it reads from inscription rows.
- On V1.0: the bytes and content type of all 578 inscriptions, the shims, the routes, the app window's
  doors and the App Store list. GET requests only.

## What already matches

- All 577 Pepecoin-testnet inscriptions come back from V1.0 with the same SHA-256 and content type.
- The app frame's sandbox and content policy equal the original's.
- All 12 shims are served.
- Inscription rows, `/r/wallet`, `/r/profile` and `/r/tag` have the original's shape, except the two
  fields in gaps 3 and 5.
- V1.0's App Store lists 23 of the 26 (gaps 9 and 14 explain the other three).

## Result

| | Apps |
|---|---|
| Run fully | 3 |
| Run with gaps (the single-player or text part works) | 14 |
| Broken | 8 |
| Not runnable on the original either (loads a file that is not an inscription) | 1 |

Apps by first 8 hex digits of their card:

| Card | Chain | Needs beyond storage and plain rooms | V1.0 | Gaps |
|---|---|---|---|---|
| 5d7b8c27 | peptest | inscription reads | works | — |
| 189f9729 | peptest | — | works | — |
| 20974442 | peptest | — | works | — |
| 561b6628 | peptest | fast lane for netplay | solo only | 1 |
| c0569d70 | peptest | fast lane for netplay | solo only | 1 |
| e1b21203 | peptest | fast lane, media | solo only | 1, 2 |
| 882a8647 | peptest | fast lane (positions travel only over it) | multiplayer broken | 1 |
| 0cc36b40 | peptest | fast lane | multiplayer broken | 1 |
| 19a9ee72 | peptest | fast lane | multiplayer broken | 1 |
| e209554d | peptest | fast lane with room fallback, ring, contacts | gaps | 1, 7 |
| 8420346e | peptest | fast lane with room fallback, `/r/send` | gaps, hidden | 1, 14 |
| 7092dfd7 | peptest | groups, fast lane, media, moving area cells, ring | text chat only | 1, 2, 7, 12, 13 |
| 1d2b7c4e | peptest | player trades, `held`, rooms to a payout program | gaps | 3, 4 |
| 2a43ddfe | peptest | as above, plus referee seeds | gaps | 3, 4, 6 |
| d0c34006 | peptest | as above | gaps | 3, 4, 6 |
| 3f8be7cd | peptest | `collection` and `held` in rows | gaps (a prize is never found) | 3, 5 |
| 81389ff2 | peptest | claim door, claim-pool reads, `collection` | gaps (the game plays) | 5, 10 |
| b75d160f | peptest | fast lane, screen media, ring | broken | 1, 2, 7 |
| 1ddd03f8 | peptest | fast lane, voice | broken | 1, 2 |
| 3e5958ac | peptest | fast lane, camera and voice | broken | 1, 2 |
| acfe0f6d | peptest | `/r/collection/<creator>/<name>` | broken | 5 |
| ae5803de | peptest | `/r/sites`, `/r/site` | broken | 8 |
| 3e876e81 | peptest | `/r/mesh` | broken | 8 |
| 7f02bc78 | peptest | `/r/mesh` | broken | 8 |
| 32758ebc | pep main | rooms, inscribe, referee seeds, `/r/verified` | broken | 6, 9, 11 |
| b9d23fc4 | peptest | a registry file that is not an inscription | not runnable on either | 14 |

## Gaps, by how many apps each blocks

1. **Fast lane — 12 apps.** A room joined with `{fast: true}` gets `fast: false` and `room.fast` is null
   (`arcade-desktop/dist/appwindow.js:187`, `arcaded/src/shims/realtime.js:28`). The original always gives
   a fast object, and carries it over the room when peers have no direct link.
   - Step 1 (**M**): a fast object with the original's interface that runs over the room. Every app that
     only needs the interface works at once; ones that need the rate get it in step 2.
   - Step 2 (**L**): a direct lane between the two arcades, with the room as the fallback. It cannot be
     data channels opened by the app window: on Linux the window has no `RTCPeerConnection` (see "Built so
     far"). The daemon carries it: its own UDP lane between two V1.0 arcades, and a WebRTC data channel
     made by the daemon to reach a browser on an original arcade. The app window hands fast sends to the
     daemon and gets them back as events, as it does for room messages.
2. **Room media (camera, microphone, screen) — 5 apps.** The door refuses `media` (`appwindow.js:202`).
   The window can capture (`getUserMedia` is there) but cannot send, so the media lane is the daemon's
   too, on top of gap 1 step 2. **L**
3. **`held` means the wrong thing — 4 apps.** In the original, `held` on an inscription row means "this
   arcade has the content". V1.0 sets it to "this wallet owns it" (`arcaded/src/walletsvc.rs:33`): true on
   the original and false on V1.0 for every id sampled. Set it the original's way. **S**
4. **Player trades (`swap` door) — 3 apps.** The shim rejects every call. This is step 3 and 4 of
   `docs/MARKET.md` (the `swap` door needs one-transaction legs). **L**
5. **Collections — 3 apps.** `collection` and `edition` are always null in rows (`walletsvc.rs:28,34`) and
   `/r/collection/<creator>/<name>` is missing. Fill both from the ledger and add the route. **M**
6. **Referee seeds (`seed` door) — 3 apps.** A seed service and the door. **M to L**
7. **`ring` / `ringed` — 3 apps.** Both are stubs (`appwindow.js:200-201`). Rings over the mesh and a
   desktop notification. **M**
8. **Mesh web reads `/r/sites`, `/r/site`, `/r/mesh` — 3 apps.** Build them on V1.0's names index. **M**
9. **Apps on a chain other than the home chain — 1 app.** `/content` and the page API answer only the home
   chain (`arcaded/src/http.rs:219,293`), and a borrowed chain's apps aren't listed
   (`arcaded/src/appsvc.rs:44`). Take the chain from the app frame's address. **M to L**
10. **Claim door and `/r/claimpool(s)` — 1 app.** **M to L**
11. **`/r/verified/<judge>` — 1 app.** **M**
12. **Area cells that move — 1 app.** `area()` joins one cell and has no `move()`
    (`shims/realtime.js:78-82`). **S to M**
13. **`groups.save` — 1 app.** Refused at `appwindow.js:165`. **S to M**
14. **Lookalike flags — 2 cards hidden.** The operator clears them (`apps.review`, `apps.trust`); check why
    V1.0 flags a pair the original shows. **S**

## Smaller findings

- **Contacts door reply:** V1.0 answers `{people}` where the original answers `{contacts}`. No deployed app
  breaks (all use the shim), but the door should answer the original's name too. **S**
- **Saves don't carry over** from an original arcade: both key saves by page txid, so an import is possible.
- **Not used by any deployed app:** the model door, escrow, `state.update`, `take`, `mint`. They stay on
  the list for the app API, behind everything above.

## Not verified

- **App windows on Linux run in WebKitGTK.** Checked so far on one machine (see "Built so far"): WebGL
  and data channels. The Gamepad API and `DecompressionStream` are not checked. Needs a run of each app in
  a real V1.0 app window, not a code reading.
- **Apps that talk to a program over a room** (a payout service on another computer): the room path from
  V1.0 over the mesh to that program was not exercised.
- **One app was run in two seats on V1.0** (card 882a8647, see "Built so far"); no other was. For the
  rest, "works" means every dependency found in its code is answered the original's way.

## Build order

Martin, 2026-11-10: this work comes before the market plan (`docs/MARKET.md`).

1. Gaps 3 and 14 and the contacts reply (**S** each): four apps' own-content checks and two hidden cards.
2. Gap 1 step 1 (**M**): the fast object over the room. Twelve apps get their multiplayer interface.
3. Gap 5 (**M**) and gap 8 (**M**): six apps.
4. Gap 7, 12, 13 (**M**, **S**, **S**). Built.
5. Gap 1 step 2 and gap 2 (**L**, **L**): the daemon's direct lane, then media. Five apps. Built between V1.0 arcades; original arcades not reached.
6. Gap 4 with `docs/MARKET.md` steps 3 and 4; gap 6, 10, 11.
7. Gap 9. Built; not run end to end (see Step 7).

## Built so far

On branch `claude/app-compat`, not committed, not installed on any live arcade.

- **Step 1.** `held` is "this arcade has the content"; the lookalike check compares a card only with apps
  the store still lists; the contacts door answers `contacts` as well as `people`.
- **Step 2.** A room joined with `{fast: true}` gets a fast object with the original's interface, carried
  over the room as hidden room messages (`appwindow.js`, `shims/realtime.js`). The limits are the
  original's own: a pump every 340 ms, reliable sends in order, only the latest plain send, nothing over
  400 characters.

What was run, on a test arcade off the mesh, two seats through a stand-in for the desktop app's bridge:

| Check | Result |
|---|---|
| 30 checks of the fast object (peers, plain and reliable sends, bytes, order, rejoin, leave) in Chromium | 30 pass |
| The same 30 in WebKitGTK 2.52.6, the engine of the desktop app's windows | 30 pass |
| Card 882a8647 in two WebKitGTK windows | both join one match; the keeper's scoreboard reaches the other seat; each seat sends about 30 fast messages a second and hears about 3 |

What that shows:

- Over the room alone a seat hears about 3 updates a second. That carries a lobby, turns and scores; it
  is not enough for fast action. The rate comes with gap 1 step 2.
- **WebGL** in a WebKitGTK window on this machine (aarch64, NVIDIA, X11) works only with
  `WEBKIT_DISABLE_DMABUF_RENDERER=1`. The installed desktop launcher sets it.
- **`RTCPeerConnection` does not exist** in a WebKitGTK window here, with the WebRTC settings on;
  GStreamer has no `webrtcbin`. `getUserMedia` and `AudioContext` exist. This is why gap 1 step 2 and
  gap 2 moved into the daemon.
- Pointer lock needs the window to have focus.

Not yet run for steps 1 and 2: a desktop app window built from this branch (the runs above used the same
page code behind a stand-in bridge), and two seats on two arcades, one of them an original.

### Step 3: collections (gap 5) and sites on names (gap 8)

**Collections.** Inscription rows carry `collection` and `edition`, and `/r/collection/<creator>/<name>`
is served, both from the ledger. Compared with an original arcade on the same chain: 29 collections,
3015 items and 113 single rows, 0 differences.

**Sites on names.** The rules are the original's: a site record is an ord JSON inscription
`{"p":"mesh","name":…,"pages":{…}}`; a name's site is the newest record sitting at the address that
holds the name's winning claim; `.mesh` names live on Pepecoin, other names on any chain.

- Served, to callers on this computer: `/r/mesh`, `/r/mesh/<address>`, `/r/sites`, `/r/site/<address>`
  (`?chain=`, `?have=`), the drawn page `/site/<chain>/<address>`, and ord content at
  `/content/<inscription id>`.
- Where the answer comes from: this arcade when it keeps the chain's names index, otherwise a V1.0 arcade
  on the mesh that does, through two new Explorer commands (`explorer.site`, `explorer.sites`).
- No new index. The records are found by a background scan of the ord index, kept in
  `cache/sites-<chain>.json`; the newest 4096 inscriptions are always read live.
- An arcade on shared storage that keeps only part of a chain's ord groups does not answer for sites.

What was run:

| Check | Result |
|---|---|
| The parsing and naming rules against 362 records and the name, title, picture and address cases, all produced by the original's own code | 0 differences (one found and fixed: a number as a name) |
| A made-up chain through the real chain store and ord and names indexes: claim, records by the holder and by others, a later losing claim, a moved claim | pass |
| A real chain: a name and two records inscribed on Pepecoin testnet with the original's inscriber, read back over HTTP from a test arcade | the newest record is the site; text, HTML and inscription pages, the picture line, the missing-page and unknown-name answers and `?have=` all answer as written |
| Card ae5803de in a WebKitGTK window on that test arcade | lists the site and shows its page with the picture |

Not served: `/r/mesh/picture/<id>`, `/r/search`, and asking original arcades when no V1.0 arcade keeps
the index. Links in a drawn page point under `/site/<chain>/`.

Not yet run: cards 3e876e81 and 7f02bc78 (they read `.mesh` names, which need a V1.0 arcade that keeps
Pepecoin's names index and runs this branch), the answer fetched from another arcade over the mesh, and
the refusal of callers that are not this computer (the test arcade listens on this computer only).

### Step 4: rings (gap 7), area cells that move (gap 12), groups.save (gap 13)

**Rings.** Ported from the original: a ring is said in the room `arcade!ring/<address>` and carries the
caller, its certificate, its arcade's node id, the callee, the app's page, a line and a link. The
recipient's arcade keeps it only if it is well formed and for this address through the arcade it names,
the caller is in the address book, the app is installed with Notifications on, the certificate checks, and
the pair is within its limits (one every 15 s, 30 an hour; a caller sends 20 a minute). It is kept 10
minutes. An arcade holds its ring room only while the desktop window asked `ring.waiting` in the last
20 s and some app has Notifications on. Page rooms cannot name a ring room (`!` is not allowed in a game).
The main window shows a card (who, which app, the line; Not now / Open); Open starts the app with the ring,
and `ringed()` in that window gives `{from, tag, link, line, at}`, null in any other window.
Commands: `ring.send`, `ring.waiting`, `ring.get` (`docs/API.md`).

**Area cells.** `area(base, {cell, view, x, y})` joins the cells it can see and `move(x, y)` changes them
as the player walks; it speaks only in its own cell.

**groups.save.** Saves to Downloads, never over another file (`name (1).txt`); a path in the name stays a
name. The first save in a window goes through with a note; the next asks; No answers `declined`. Off
unless the app's Group chats switch is on. Resolves `{ok: true, saved, arcade: "groups", seq}`.

Findings:
- **A ring carries about 145 bytes of line and link.** Room messages are 512 bytes and the ring's fixed
  part takes the rest. The original drops a longer ring without a word; V1.0 answers the app an error
  ("too long: … N bytes more than a ring carries").
- **A player who had left was listed again for up to 24 s.** When an arcade's last player left a room it
  kept what it knew of the others, and a leave after that never reached it; on coming back the gone player
  was in the member list until it expired. Now an arcade forgets a room's other players when its last own
  player leaves, and learns them afresh on the next join. The original keeps them the same way (10-24 s).

What was run (two test arcades from this branch in a private network, linked only to each other):

| Check | Result |
|---|---|
| Rings through the daemon: delivered, away, no Notifications, stranger, same-app `ring.get`, pair gap, too long, self / no page / no address, a page naming a ring room, caller limit, ring room let go | 24 of 24 |
| The desktop pages in WebKitGTK behind a stand-in bridge: ring card, Open, `ringed()`, Not now, the real inscribed app opened from a ring; every groups.save case; two players on two arcades walking between cells and along an edge | 36 of 36 |
| Unit tests: ring shape and limits, line cleaning, a saved file never replacing another | pass |

Not yet run: a ring from a wrong node or with a forged certificate live (cannot be made through the API;
unit-tested), the real desktop window (Tauri) saving into Downloads, opening an app from a ring and the
system notification, and rings to or from an original arcade.

### Step 5: the fast lane between arcades (gap 1 step 2) and room media (gap 2)

**Fast lane.** A room joined with `{fast: true}` now also asks its arcade for the fast lane
(`realtime.join` with `fast`). The arcades carry fast sends themselves (`crates/arcaded/src/fastlane.rs`):
- a signed `fx` frame goes to the arcade the player plays through, over the link to it when there is one,
  else along the room's routes, passed on by V1.0 arcades;
- two arcades say hi / ho; a player whose arcade answered within 10 s has a lane;
- the window (`appwindow.js`) moves a pair to `direct` when `realtime.lanes` lists the player. It sends
  to them with `realtime.fast` (to, data or bytes, reliable), one call at a time, so reliable sends keep
  their order.
- Unreliable sends waiting to go out are replaced by the newest; reliable ones are never dropped (a "too
  fast" answer is tried again).
- Players with no lane stay on the room, as in step 1; that covers original arcades and older V1.0 ones.
- Limits: 16 KB a message, about 240 a second per seat. While the lane is busy, links send what is queued
  every 2 ms instead of 50.

**Room media.** The window does what the original's viewer does with WebRTC tracks, with WebCodecs
(`dist/media.js`, `dist/media-worklet.js`):
- it opens the camera, microphone or screen and encodes VP8 pictures and Opus sound;
- the arcade carries the pieces (`realtime.media`, up to 60 KB, split above 48 KB) over a direct link
  between the two arcades, or, when they have none (both behind NAT), through at most two arcades on
  the room's route whose media relay is on (as the original's media went through a node's TURN relay);
- the other window decodes them and draws each picture where its page placed it, and plays each voice
  the page hears.
- Ops: start, stop, screen, share, hear, volume, place, mute, frames, levels, spatial, shown. Events:
  media, media-off, state, speaking, frame, levels.
- The pill with Stop shows while anything is open. The app's card switches (Camera & microphone,
  Pictures in the app) gate it.
- App windows of the desktop app turn on WebKitGTK's media capture and allow its camera and microphone
  requests (`allow_media` in `arcade-desktop/src/main.rs`). The app's sandboxed frame cannot open the
  camera itself.
- Pictures need `WEBKIT_DISABLE_DMABUF_RENDERER=1`, which the installed launcher already sets: without
  it a `<video>` never plays a camera stream on this machine, so nothing can be encoded.

Findings:
- **Room frames were lost across a relaying arcade (on main too).** An arcade that borrows blocks or
  fetches shares from a neighbour holds more connections to it under the same id. Room frames for that
  neighbour were taken by whichever connection's loop came first, including ones whose reader drops
  them. When such a connection closed, every room route through that neighbour went with it. Three
  arcades in a line heard nothing of each other. Now room and lane frames go out only on the connection
  the neighbour's own room frames came in on, and routes go only with the last connection.
- **The same pattern remains in `udpsvc.rs` (not changed).** UDP-path state is kept per neighbour id, so
  a borrowing connection replaces the mesh link's UDP state and removes it when it closes. It is
  wire-shared with the original, so it is left for its own change.

What was run (three test arcades from this branch in private networks: t1 and t2 linked, t3 linked
only to t2):

| Check | Result |
|---|---|
| Lane at the daemon: lanes up, direct and relayed; 60 a second for 5 s; 200 reliable in order; bytes; naming; limits; a player who left; media direct only, never relayed, size limit | 22 of 22 (unreliable: 291 of 293, median 7.6 ms) |
| Ordinary room messages across t2 between t1 and t3 | both ways (none before the fix) |
| Lane in two WebKitGTK app windows: direct, 150 unreliable, 300 reliable at once, bytes, both ways, leaving | 15 of 15 (150 of 150, median 17 ms; 300 reliable in 0.4 s; the room carried about 3 a second) |
| Media in two WebKitGTK app windows, with a generated picture and tone in place of a camera: start, nothing before share, share, place, the picture's colour, hear and speaking, frames, sound only, mute, Stop on the pill, screen, hideScreen | 20 of 20 |
| Steps 1 to 4 again (rings 24, step-4 windows 36), all unit tests | pass |

Not yet run: a real camera, microphone or screen (none on this machine; screen sharing in WebKitGTK
goes through the desktop portal), the real desktop window (Tauri), sound actually heard (the test
checks levels, not the speaker), echo cancelling, media kept across a seat taken again (it stops),
and anything with an original arcade.

**Not built: reaching a browser on an original arcade.** Its players link over WebRTC (data channels,
and media tracks on the same connection); a V1.0 arcade would need a WebRTC stack of its own (ICE,
DTLS, SCTP, RTP) to answer them. Until then such pairs use the room for fast sends (as step 1), and media
between them does not work.

### Step 6: player trades (gap 4), verified results (gap 11), prize pools and seeds (gaps 10 and 6)

**Player trades (`arcade.swap.trade`, `answer`, `cancel`, `items`, `onTrade`).** The original's protocol
message for message (`dist/trade.js`): hidden room messages `"\u001farcade-trade:"` (propose, accept,
decline, cancel, leg in sealed parts, done, failed), each player's own yes on a card, the side giving an
inscription signs. The arcade does the wallet's part (`crates/arcaded/src/tradesvc.rs`,
`crates/arcade-wallet/src/legs.rs`, `legacy.rs`):
- the leg: two of the seller's coins, the swap's OP_RETURN and the seller's payment, both signed
  SINGLE|ANYONECANPAY, sealed to the buyer's messaging key (the original's envelope);
- the completion: the fee reservation back to the seller, the buyer's coins signed ALL, change;
- checked before completing, beyond the original: each of the leg's signatures.
Equal to the original byte for byte: 60 signature-hash vectors and 8 legs (payload, fee, payment, raw
transaction, both digests) made by the original's own `funding.py`; 20 fee vectors.
Bundles (several things a side) are not built.

**`/r/verified/<judge>`** (`refereesvc.rs`): inscriptions carrying a referee's signed result for a judge,
checked here (signature, content, creator); attestations made by the original's referee code check.
A V1.0 arcade runs no judges: the trusted referee is named by `?referee=` or the operator's
`referee.set`.

What was run (two test arcades in a private network following Pepecoin testnet through the local node,
`ARCADE_COIN_PEERS`; @claude's test wallet and a test wallet, never one of Martin's):

| Check | Result |
|---|---|
| Daemon: t2 sells inscription #7063 to @claude for 0.5 test PEP; a wrong price, the wrong player and swapped signatures refused | sent; accepted by pepecoind; settled; the live arcade's ledger shows the new owner |
| Two WebKitGTK app windows, every card pressed: offer, accept, signed, finish with its fee, broadcast | 13 of 13, settled on testnet |

Found: a test arcade's shared pool kept a transaction the network never had, and the arcade treated its
output as spendable (the first trade needed that parent relayed by hand).

**Prize pools, the claim door and seeds (gaps 6 and 10)** (`crates/arcaded/src/prizesvc.rs`, doors `seed` and `claim`
in `appwindow.js`). A V1.0 arcade runs no judges; it is a client of the referee arcade a pool names:
- pools are read from the ledger's inscriptions (`application/vnd.arcade.prizepool`), with the original's checks
  (creator, the two-key address its keys make, deletions); `/r/prizepool`, `/r/claimpools`, `/r/claimpool` in the
  original's shapes;
- a claim opens the lot with the page's phrase (AES-256-GCM), rebuilds the lot's leg from the pool's record, checks
  the pool key's signatures, adds the claimer's coins (or, for a pool that pays, only the claimer's output), and for
  a refereed pool asks the referee (`/r/referee/sign`, the run as `replay`) for its signatures over the whole claim;
  the claim door says `heard` at once, as the original;
- the seed door asks the pool's referee (`/r/referee/seed`) for a seed issued to this arcade's player;
- a pool naming no referee address (the arcade it was made on) is reached through the operator's `referee.set`
  when that arcade's key is the pool's. The space shooter's live pool names none; its referee is the original arcade
  at app.dogecoinarcade.com.
- Found and fixed: sends could not pay a P2SH address (a refereed pool's), "an address on another chain".

| Check | Result |
|---|---|
| `/r/prizepool`, `/r/claimpools`, `/r/claimpool` for the live pool and four test pools (open and claimed) | equal to the original arcade's answers |
| Daemon: claims of a pool the claimer pays (0.1 test PEP) and of a refereed pool that pays, with a seed; wrong phrase, no run, a seed the referee never issued, a second claim | sent, accepted by pepecoind (its script checks included), settled; refusals as written |
| An app window: a claim with its card; a seed then a refereed claim, no card; nothing left after; a wrong phrase | 5 + 5 of 10, settled on testnet |

The refereed runs used a stand-in referee built from the original's referee code (no judge: every run wins).

Not yet: a claim from a real original referee (it needs a winning run, and the original arcade is read-only for
these tests), session seeds beyond passing `session`, the "once per wallet" check on this side (the referee and
the chain's pool rules still apply), and a trade with a player on an original arcade.

### Step 7: apps on a chain other than the home chain (gap 9)

An app frame's address names its chain (`/app/<chain>/<txid>`), and its requests carry that address as
their Referer. So:
- **the app's page** comes from this arcade's ledger of that chain, or, for a chain it only borrows, from
  an arcade on the mesh that keeps the chain's ledger (`explorer.ledger_bytes`, checked against its
  sha256, kept in memory);
- **its reads** (`/r/...`, `/content/<id>`) answer on that chain (`crates/arcaded/src/pagechain.rs`): from
  this arcade's ledger, or answered by such an arcade (`explorer.page`, public reads only, never the
  asking arcade's wallet or send requests); `/r/wallet` is this wallet on that chain (its addresses there,
  its coins there);
- **writes stay on the home chain**: a payment (`/r/send`) or a door that writes with the wallet
  (inscribe, trades, seeds, claims, owner send) from an app on another chain is refused, saying so;
- **the App Store** lists the apps of chains this arcade only borrows, from an arcade that keeps their
  ledger (`explorer.app_cards`, asked in the background every 10 minutes).
- `ARCADE_COIN_PEERS` takes `net=host:port` (a peer for one chain).

Tests: unit tests for the frame's chain, the page reads another arcade may answer, per-chain coin peers.

Not run end to end: no arcade on the mesh keeps Pepecoin mainnet's arcade ledger today (the one deployed
app on another chain is inscription #2 there), so a borrowed-chain app has nothing to load from. A test
arcade began syncing mainnet in a private network and was stopped at block 753,000: a full mainnet copy
is about 17 GB, more than this computer had free. The arcade ledger on mainnet starts at block
1,208,000, so an arcade that already keeps mainnet needs little more for the ledger index.

### The explorer for apps

`GET /r/explorer/<name>?args=<json>` (`explorer::page_read`, docs/API.md "The explorer for apps"): the
public reads of V1.0's Explorer, for an app that shows the same explorer (the Arcade Explorer app is
V1.0's `explorer.js` unchanged, reading through it). No pass needed; mesh-internal explorer questions
are not answered. `explorer.inscription_types` is now a listed public command, so `/api/v1` answers it
too. Test: `http::tests::apps_read_the_explorer`.

## Test that decides "done"

Every card above opened in a V1.0 app window beside the same card on an original arcade, two seats where
the app has more than one: the same screens, the same saves behavior, the same room traffic, and each
door op the app uses answered with the same fields. One checklist row per card and op, kept in this file.
