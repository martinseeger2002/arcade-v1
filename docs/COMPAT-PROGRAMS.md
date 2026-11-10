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
| `/inscriptions/<id>/full` | redirects to the sandboxed `/app/<home chain>/<id>` |
| `/realtime/hello`, `join` (certificate signed by the address), `send`, `leave`, `stream` (server-sent events, chunked) | the room door, on V1.0's mesh rooms |
| Core JSON-RPC: `getblockcount`, `getrawtransaction` (verbose, Core's shape), `decoderawtransaction`, `gettxout`, `validateaddress`, `importaddress`, `listunspent`, `listtransactions`, `sendrawtransaction` (checked like a node, then shared over the mesh and sent to coin peers) | `POST /` (home chain) or `POST /chain/<key>`, Basic auth with `<home>/.cookie` |

## Pointing a program at V1.0

- The arcade's address: `http://127.0.0.1:<http port>` (not a public name through a proxy:
  these routes answer only this computer).
- The node's RPC: the same address; the cookie file is `<arcade home>/.cookie`.
- Coins for an address come from `listunspent` (V1.0's address index); no wallet import or
  rescan is needed (`validateaddress` reports every address as watched).

## Checked (2026-11-10, a copy of a live testnet chain, with the original's own client code)

- `listunspent` for an address with 270 coins: the same coins and total as a Core node.
- The original's transaction decoder over `getrawtransaction`: 1,500 transactions read the same
  through V1.0 and Core (101 token sends, 3 grants).
- `/r/balances/<addr>?pending=1`: 55 tokens equal to the original's.
- `/r/inscriptions/<addr>`, `?creator=`, `/r/inscription/<id>`, `/r/tx/<txid>`: same fields and ids.
- The original's certificate room client: joined, messages arrived in ~10 ms, a wrong key was
  refused.
- `sendrawtransaction`: a transaction already mined -> -27, an unknown coin -> -26.
