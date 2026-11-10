# The ASHVALE Bank on V1.0

The Bank (`tools/bank/bank.py` in the private ashvale-bank repo) runs on the same computer as
its arcade and talks to it at `http://127.0.0.1:<http port>`. V1.0 answers everything it asks
of the original arcade and of a Pepecoin testnet Core node, from this computer only.

## What it uses

| The Bank asks | V1.0 |
|---|---|
| `/r/tag/<tag>`, `/r/token/<id>`, `/r/inscription/<id>`, `/r/inscriptions/<addr>`, `/r/inscriptions?creator=`, `/r/tx/<txid>`, `/content/<id>` | page API (same fields as the original) |
| `/r/balances/<addr>?pending=1` | `units` from the blocks, plus `arriving` / `leaving` per token from the mempool (simple sends and grants) |
| `/r/pending/<addr>` | waiting token transfers in and out |
| `/inscriptions/<id>/full` | redirects to the sandboxed `/app/<home chain>/<id>` |
| `/realtime/hello`, `join` (certificate signed by the address), `send`, `leave`, `stream` (server-sent events, chunked) | the room door, on V1.0's mesh rooms |
| Core JSON-RPC: `getblockcount`, `getrawtransaction` (verbose, Core's shape), `decoderawtransaction`, `gettxout`, `validateaddress`, `importaddress`, `listunspent`, `listtransactions`, `sendrawtransaction` (checked like a node, then shared over the mesh and sent to coin peers) | `POST /` (home chain) or `POST /chain/<key>`, Basic auth with `<home>/.cookie` |

## Settings for the Bank

```
ASH_APP=http://127.0.0.1:<http port>          # the arcade (no Cloudflare in between)
ASH_NODE_RPC=http://127.0.0.1:<http port>     # V1.0 answers Core's calls itself
ASH_NODE_COOKIE=<arcade home>/.cookie
ASH_BANK_COINS=node                           # coins from listunspent (V1.0's address index), not the old ledger sqlite
```

`ASH_ARCADE` still points at a dogecoin-arcade checkout: the Bank builds and signs its payouts
with that code (`arcade.publish`), as before. Only the arcade *service* is replaced.

## Checked (2026-11-10, a copy of the GX10's chain)

- `listunspent` for @ashvale: 270 coins, 10,111.08756 PEPtest, the same as the GX10's Core.
- The Bank's own decoder (`arcade.tx.extract` over `getrawtransaction`): 1,500 of @ashvale's
  transactions read the same through V1.0 and Core (101 token sends, 3 grants).
- `/r/balances/@ashvale?pending=1`: all 55 tokens equal to the original's (GOLD #26: 94,828).
- `/r/inscriptions/<addr>`, `?creator=`, `/r/inscription/<id>`, `/r/tx/<txid>`: same fields and ids.
- `cert_room.py` (unchanged) joined a room by certificate, messages arrived in ~10 ms, a wrong
  key was refused.
- `sendrawtransaction`: a transaction already mined -> -27, an unknown coin -> -26.
