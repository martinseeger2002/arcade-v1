# Programs written for the original arcade

A program that ran next to the original arcade and a coin node (a payout service, a game
server, a bot) can run next to V1.0 instead, on the same computer, talking to it at
`http://127.0.0.1:<http port>`. V1.0 answers what such programs ask of the original arcade and
of a Core node, from this computer only.

## What V1.0 answers

| A program asks | V1.0 |
|---|---|
| `/r/tag/<tag>`, `/r/token/<id>`, `/r/inscription/<id>`, `/r/inscriptions/<addr>`, `/r/inscriptions?creator=`, `/r/tx/<txid>`, `/content/<id>` | page API (same fields as the original) |
| `/r/balances/<addr>?pending=1` | `units` from the blocks, plus `arriving` / `leaving` per token from the mempool (simple sends and grants) |
| `/r/pending/<addr>` | waiting token transfers in and out |
| `/r/ledgertx/<txid>` | the arcade ledger's record of one transaction (the original's `arcade_tx` row): `block`, `position`, `class`, `type`, `version`, `sender`, `reference`, `payload_hex`, `valid`; 404 with `state` `pending` (mempool, or a block the ledger hasn't reached), `none` (no arcade record) or `unknown`. No `invalid_reason`: V1.0 keeps only valid or not |
| `/inscriptions/<id>/full` | redirects to the sandboxed `/app/<home chain>/<id>` |
| `/realtime/hello`, `join` (certificate signed by the address), `send`, `leave`, `stream` (server-sent events, chunked) | the room door, on V1.0's mesh rooms |
| Core JSON-RPC: `getblockcount`, `getrawtransaction` (verbose, Core's shape), `decoderawtransaction`, `gettxout`, `validateaddress`, `importaddress`, `listunspent`, `listtransactions`, `sendrawtransaction` (checked like a node, then shared over the mesh and sent to coin peers) | `POST /` (home chain) or `POST /chain/<key>`, Basic auth with `<home>/.cookie` |

## Pointing a program at V1.0

- The page API (`/r/...`) answers an app page with its page pass, or a program on this computer that sends the
  RPC cookie (`<home>/.cookie`) as Basic auth, as `/rpc` and the Core RPC take it: reads only, on the home chain;
  `/r/send` and the market's writes still need an app page's pass.
  A program reading for an app on another chain adds `X-Arcade-Chain: <key>` (a chain this arcade keeps or
  borrows): those reads answer on that chain, as an app page's pass makes them.

- The arcade's address: `http://127.0.0.1:<http port>` (not a public name through a proxy:
  these routes answer only this computer).
- The node's RPC: the same address; the cookie file is `<arcade home>/.cookie`.
- Coins for an address come from `listunspent` (V1.0's address index); no wallet import or
  rescan is needed (`validateaddress` reports every address as watched).

## The media relay

Every V1.0 arcade with a STUN port runs a media relay on it (TURN, UDP, `turnsvc.rs`), wire for wire the
original's `arcade/mesh/turn.py`: when two browsers in a call can't reach each other, one sends through
it. It is offered only where other arcades proved the STUN port open, so in practice by arcades on public
addresses (a VPS); one behind CGNAT borrows: `realtime.ice` asks linked arcades ("tu") for a login on
theirs, the address always from its own proof of their STUN port. A website gives its pages
`realtime.ice`'s `iceServers` (no Cloudflare). Limits as the original's: 256 allocations, 32 per login,
12 Mbit/s each, 200 Mbit/s in all, 100 GB a month unless set (`mesh.udp_set`); never to this machine,
and into a private network only for a client on one. `ARCADE_MESH_TURN=0` or `turn: false` turns it off.

## Borrowed chains

A chain this arcade borrows (not installed, or installed but still syncing or indexing, the home
chain included) answers the Core JSON-RPC at the same address (`POST /` for the home chain,
`POST /chain/<key>`), from its lenders:

| Call | On a borrowed chain |
|---|---|
| `getblockcount`, `getbestblockhash`, `getblockchaininfo` | the borrowed copy's tip (every block from the floor on checked by proof of work) |
| `listunspent` (addresses named) | a lender's address index (`arcade_coinscan`, which an arcade with the chain installed answers for any address at once); each coin checked against its block, then asked of the lender whether it is unspent. At most 500 new coins are checked per call (the largest first); a lender names at most 5,000 per address |
| `getrawtransaction` | the lender's copy, checked against its txid; its block found among the headers kept here and the transaction found in it |
| `gettxout` | the lender's word |
| `sendrawtransaction` | to every lender's node and the coin's P2P peers; refused by all, an error |
| `decoderawtransaction`, `validateaddress`, `importaddress` | answered here (nothing needs watching) |
| `listtransactions` | `[]` (nothing is watched for a program) |

A call the borrowed side doesn't answer falls back to the copy kept here, when there is one.
`tx.fee`, `tx.coins`, `tx.broadcast` and the explorer's addresses work the same way on a borrowed
chain; `tx.activity` (history) still needs the chain kept here.

**Where an answer comes from.** `getrawtransaction` (verbose), `gettxout` and each `listunspent` row
carry `checked` and `from`, on a kept chain and a borrowed one, for a program that must not act on a
lender's word (a swap checking a lock):

- `from`: `"own copy"` (the chain kept here), `"borrowed (checked blocks)"` (a borrowed chain, in a
  block this arcade checked by proof of work on its own header chain), `"lender"` (the lender's word
  only), `"sent from here"` (a waiting output of a transaction this arcade broadcast).
- `checked`: `true` only when the answer rests on a block this arcade checked itself, on its header
  chain, with the transaction found in it (`getrawtransaction`) or the coin found in it (`listunspent`).
  A transaction only in a mempool is `checked: false`, 0 confirmations. `gettxout` on a borrowed chain
  is always `"lender"` (whether a coin is still unspent is the lender's word).
- `height` (and `blockhash` for a transaction): the block it is in.

## Checked (2026-11-10, a copy of a live testnet chain, with the original's own client code)

- `listunspent` for an address with 270 coins: the same coins and total as a Core node.
- The original's transaction decoder over `getrawtransaction`: 1,500 transactions read the same
  through V1.0 and Core (101 token sends, 3 grants).
- `/r/balances/<addr>?pending=1`: 55 tokens equal to the original's.
- `/r/inscriptions/<addr>`, `?creator=`, `/r/inscription/<id>`, `/r/tx/<txid>`: same fields and ids.
- The original's certificate room client: joined, messages arrived in ~10 ms, a wrong key was
  refused.
- `sendrawtransaction`: a transaction already mined -> -27, an unknown coin -> -26.
- `/r/ledgertx/<txid>` (2026-11-11): 45 transactions (30 valid, 15 invalid, sends to and from one
  service address, the oldest and newest) equal to the original's `arcade_tx` rows field for field.
