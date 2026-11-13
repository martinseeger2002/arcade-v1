# Arcade V1.0 — Plan

Status: **plan only, nothing built.** Drafted 2026-11-07.
Arcade V1.0 is a from-scratch rebuild of Dogecoin Arcade, in Rust, for Linux PCs.

## Status (2026-11-07)

| Milestone | State | Verified against |
|---|---|---|
| 1. Store + headers/blocks over P2P | done | Pepecoin testnet 1.1.0, DigiByte 9.26.7, Kotia 1.0.0, Litecoin 0.21.5.8 live peers; byte-identical blocks |
| 2. Coins, spends, undo, reorgs | done | unit tests; **full testnet synced**: 1,561,536 blocks, 1.63 M txs, 4.56 M coins in 47 min |
| 3. Mesh client (existing protocol) | link done | linked with live Python arcades on `arcade-main`; live welcome record signature verifies |
| 4. Bootstraps | mesh-first done | empty node bootstrapped from another V1.0 arcade over the mesh (~1,600 blocks/s), P2P fallback |
| 5. Ledger port | **done** | full arcade ledger (tokens, MetaDEx, INSC NFTs, swaps, bundles, offers, asks, tags) in-commit; **d, s and n all equal the 3 live arcades** over 1,496,133-1,561,500; 6,988 inscriptions numbered identically; 119 profiles indexed |
| 6. Wallet + faucet + tag claim | **done live** | `@arcade_v1` claimed at testnet block 1,561,561 (0-conf spend of a faucet payment); old arcade 156.67.28.112 reports the same names_digest (2523f1e8...) |
| 6b. Testnet mining | **done live** | mined block 1,561,560 without Core; difficulty rule matches all 1,561,557 testnet blocks |
| 7. arcaded + CLI | running | linked to 3 live arcades, synced at tip, serves coreshare `cr` (borrower test OK) and `bk` |
| 8. Tauri desktop app | waiting | needs WebKit/GTK dev packages (sudo) |
| 9. .deb installer | not started | |

Crates: `arcade-store`, `arcade-chain`, `arcade-chaindb`, `arcade-mesh`, `arcade-ledger`, `arcade-wallet`,
`arcade-app`, `arcaded`, `arcade-cli`.

Size (testnet, 2026-11-07): chain.mdbx 4.4 GB — blocks 1.03 GB data (Core's blk files 1.3 GB), coin and address
indexes 2.5 GB on disk (1.1 GB of data: random keys leave B-tree pages ~45% full; ~4.5 M unspent class-B multisig
dust outputs). Planned: Core-style script compression, 20-byte script hashes, height-keyed headers (~3 GB).

Open items: Flopcoin's only reachable node drops non-whitelisted peers; DigiByte groestl/qubit/odocrypt PoW and
Kotia stake-kernel checks not implemented (accepted on link + merkle + heaviest chain, as the old arcade does);
DigiDollar and Litecoin MWEB parsing; old arcades pay welcome asks only by operator action, so V1.0 arcades
will auto-pay.

## 1. Scope

V1.0 is the **first-run path on Pepecoin testnet** (from Martin's whiteboard):

| # | Step | In one line |
|---|------|-------------|
| 1 | Install arcade | Installer sets up the service, data folder, both databases and the node identity |
| 2 | Connect to mesh | Find peers (built-in seed list, then on-chain announcements) and announce this node |
| 3 | Install Pepecoin test | Bring up the testnet chain in the built-in P2P chain store (no Pepecoin Core) |
| 4 | Get peptest bootstrap | Download verified testnet blocks from mesh peers |
| 5 | Get peptest index | Download the testnet **ledger** index from peers and check its fingerprint |
| 6 | Ask for handle | User picks a new tag; wallet creates a key for it (recovery phrase on first use) |
| 7 | Ask for peptest | Automatically request testnet coins from any available arcade on the mesh |
| 8 | Check handle | Is the tag free on chain and not pending in the mempool? |
| 9 | Claim handle | Build, sign, broadcast the tag claim; wait for confirmation |
| 10 | At startup start | Runs as a system service from boot, resumes sync and mesh |
| 11 | Share bootstraps | Always serve verified bootstraps and index snapshots to peers |
| 12 | CMD commands | `arcade` command line for everything above |

**Chains are add-ons.** A fresh install carries **Pepecoin testnet only**. Every other chain is an optional
add-on the user installs afterwards (`arcade chain add <key>`, or the desktop app's Chains screen); removing one
drops only that chain's keys. Add-ons the P2P core must support (requested 2026-11-07):

| Add-on | Notes |
|---|---|
| Pepecoin mainnet | AuxPoW (chain id 0x3f) |
| DigiByte + DigiByte testnet26 | Multi-algo PoW (scrypt, sha256d, skein verified; qubit, odocrypt, groestl to implement); SegWit/Taproot; **DigiDollar** (tx version 0x0770, `OP_RETURN "DD"`, DD/TD addresses) |
| Kotia | PoS (Blackcoin-style): tx nTime, block signature, scrypt block ids for header version < 7 |
| Litecoin (+ testnet4) | **All address types**: P2PKH, P2SH (both version bytes), P2WPKH, P2WSH, P2TR (bech32/bech32m), MWEB (`ltcmweb1…`); MWEB extension data kept byte-exact |
| Flopcoin | spec requested |
| More Bitcoin-family coins | A chain is a `ChainParams` entry (wire `Format` + `PowRule` + address versions); new coins are data, plus code only where a coin genuinely differs |


**Nothing is added automatically** (2026-11-07): every chain and every index is installed by hand, from the desktop
app's **Chains & Coins** tab or with `arcade chain list | add <key> [--mode light|chain|index|full] [--with ledger,ord,names,profiles]
[--snapshot] | status <key> | index add <key> <index> | remove <key>`. Both show staged progress: headers, blocks,
coins and addresses, each chosen index (ord included), fingerprints vs. the mesh. Mockup: https://claude.ai/artifact/G3T8viMzu3u4NfJAvQhJ9c

**Storage mode per chain** (chosen when adding a chain; can be changed later):

| Mode | Stores | Borrows from the mesh (verified by headers + hashes) | Good for |
|---|---|---|---|
| Light | headers only | blocks, transactions, coins, index answers | phones, tiny devices |
| Blockchain only | headers + blocks + core indexes (txids, coins, spends, every address) | nothing | serving the chain to the mesh; wallets |
| Index only | headers + chosen protocol indexes (ord, INSC, tokens, names) + coin values + content cache | new blocks (indexed, then dropped), inscription content on request | big chains (Dogecoin) on small disks |
| Full | everything: blocks + core indexes + chosen protocol indexes | nothing | full independence; serves blocks, indexes and content |

Protocol indexes can be built in the same commit as blocks during sync (`--with ord,tokens,names`), from a
verified mesh snapshot (block hash + majority of peers' chunk fingerprints, re-checked in the background), or by
replaying stored blocks later. Switching Light/Index only -> Blockchain only/Full downloads the blocks from the mesh.

**Out of scope for V1.0:** mainnet, other chains, inscriptions (Pepinals/PRC-20), trading, creating/collections,
messenger/feed/games, LLM credits. The schema carries a chain number everywhere so these add on without a rebuild.

## 2. Decisions made

| Topic | Decision |
|---|---|
| Platform | Linux PCs (built and tested on this PC: Linux Mint 22.3, 4 cores, 15 GB RAM, 2 TB NVMe). Not the GX10, not ArcadeOS for V1.0 |
| Language | Rust |
| Database | **libmdbx** (fastest open-source embedded key-value store; used by Erigon/Reth). No SQLite, no Postgres |
| Consolidation | 59 SQLite files → **2 files**: `chain.mdbx` (rebuildable) and `arcade.mdbx` (precious) |
| Chain source | Built-in P2P chain store (talks the Pepecoin protocol directly; no Pepecoin Core) |
| Mesh | **Compatible with existing (Python) arcades**: same mesh protocol, bootstraps, coins, index fingerprints |
| First peers | Built-in seed list + on-chain mesh announcements |
| Identity | Tag backed by a key in the local wallet, **no password** |
| Recovery | 12-word recovery phrase, shown once; restores every tag on any arcade |
| Tags per wallet | Multiple; each tag gets its own key derived from the phrase |
| Testnet coins | Requested automatically from any available arcade that has some |
| Index in V1.0 | Arcade ledger only (tags, tokens, balances) |
| Interface | Desktop app (**Tauri**) + CLI |
| Background | System service started at boot; always shares bootstraps |
| Code location | `~/arcade-v1` on this PC (git repo; GitHub later if wanted) |

## 3. Architecture

```
 ┌──────────────┐   ┌──────────────┐
 │ arcade-desktop│   │  arcade (CLI) │
 │   (Tauri)     │   │               │
 └──────┬───────┘   └──────┬───────┘
        │ local API (Unix socket, JSON) │
        └──────────────┬───────────────┘
                ┌──────┴──────┐
                │   arcaded   │  system service (systemd), starts at boot
                └──────┬──────┘
   ┌────────────┬──────┴─────┬─────────────┬────────────┐
 chain        ledger        mesh         wallet       store
 P2P sync,    arcade ledger  peers,       BIP39 phrase, libmdbx wrapper,
 blocks,      rules (tags,   bootstraps,  key derivation, tables, schema
 coins,       balances),     faucet,      signing,     versions,
 spends,      fingerprints   sharing      tx building  migrations
 undo, mempool
```

Cargo workspace crates:

| Crate | Job |
|---|---|
| `arcade-store` | libmdbx environment, typed tables, key encoding, schema versions and migrations |
| `arcade-chain` | Pepecoin P2P (headers-first sync, PoW + AuxPoW checks, merkle checks), block import, coins/spends/undo, mempool, broadcast |
| `arcade-ledger` | Port of the arcade ledger rules needed for tags, tokens, balances; per-block fingerprint (`index_digest`-compatible) |
| `arcade-mesh` | Existing mesh protocol: peer discovery, announcements, bootstrap and index transfer, faucet requests, serving to peers |
| `arcade-wallet` | Recovery phrase, per-tag keys, addresses, signing, building the tag-claim transaction |
| `arcaded` | The service: runs sync, mesh, wallet; serves the local API |
| `arcade-cli` | `arcade` command |
| `arcade-desktop` | Tauri app over the local API |

**One writer rule:** inside `arcaded`, one task writes `chain.mdbx` (the indexer) and one writes `arcade.mdbx`.
Everything else reads. libmdbx reads are lock-free and memory-mapped.

## 4. The P2P chain store

1. Connect to Pepecoin testnet peers directly over the Pepecoin P2P protocol.
2. Download headers, check proof of work (scrypt, AuxPoW) and the chain link; keep the heaviest valid chain.
3. Download blocks (from the mesh bootstrap first, then P2P), check each against its header's merkle root.
4. Store each block **once**, one transaction per row, and update every index in **the same commit**.
   A crash or power cut leaves the store at the last fully indexed block — the index can never be ahead of the chain.
5. Reorgs: each block keeps an undo record; disconnecting a block replays it backwards in one commit.
   Window: 1,000 blocks; deeper reorgs stop the chain and report.
6. Checked as Core checks them, on every chain:
   - every header's link, work and AuxPoW;
   - difficulty, time and checkpoints;
   - the Dogecoin-family AuxPoW header rules (early-auxpow, late-legacy, strict chain id);
   - that a block is the one its header commits to: merkle root, no repeated transactions
     (CVE-2012-2459) and the witness commitment;
   - Core's structural rules (one coinbase of 2-100 script bytes, no empty inputs or outputs,
     no null or repeated inputs);
   - every input spends a coin that exists.

   Where a chain's coin rules are known exactly (`ChainParams::value_rules`; Pepecoin's three
   networks so far), also: coinbase maturity, inputs covering outputs, MoneyRange, and a
   coinbase paying at most the subsidy plus the fees.

   A block that breaks a rule is marked invalid, with everything built on it, and the sync goes
   back to the best valid chain. A damaged copy (one that doesn't match its header) is only
   refused, since another copy may be good.

   **Not checked yet:** scripts and signatures (step 3, above an "assume valid" point per
   chain, as Core does), and the coin rules on chains other than Pepecoin. A block breaking
   those could only come from someone mining it.

   `tests/regtest` runs V1 against a real Pepecoin Core, block for block.

Testnet size today: ~1.6 M transactions, ~1.3 GB of blocks.

## 5. Data model

Key conventions: `C` = chain number (2 bytes, Pepecoin testnet = 1), `B` = local block number (4), `P` = tx position (4),
`T12` = first 12 bytes of a txid (full hash checked on every lookup; collisions go to a `*_full` table).
Integers big-endian in keys. Every table name carries a version (`core.v1.txs`). Tables are never altered in place:
a new version is built beside the old and the old one dropped.

### 5.1 `chain.mdbx` — rebuildable from the chain

| Table | Key | Value |
|---|---|---|
| `core.v1.chains` | C | genesis, P2P magic, port, address versions, PoW, AuxPoW id, floors |
| `core.v1.headers` | C + block hash | B, height, prev, 80-byte header, AuxPoW proof, total work, status |
| `core.v1.height` | C + height | block hash, B |
| `core.v1.block_tx` | C + B + P | raw transaction bytes |
| `core.v1.txs` | C + T12 | list of (B, P) |
| `core.v1.spends` | C + T12 + vout | (B, P, input index) of the spender |
| `core.v1.outputs` | C + T12 + vout | value, height, coinbase flag, script — unspent only |
| `core.v1.by_script` | C + script hash + T12 + vout | value, height — every address's coins |
| `core.v1.script_hist` | C + script hash + height + T12 | received, spent |
| `core.v1.undo` | C + B | every row the block added/removed, in order |
| `core.v1.mempool` | C + T12 | raw tx, first seen, fee, size |
| `core.v1.mempool_spends` | C + T12 + vout | spending T12 |
| `ledger.v1.arcade_tx` | C + T12 | height, position, type, version, sender, payload, valid, reason |
| `ledger.v1.property` | C + property id | issuer, name, type, supply, creation tx |
| `ledger.v1.balance` | C + address + property id | balance, reserves |
| `ledger.v1.tag` | C + tag | address, claim txid, height, position |
| `ledger.v1.tag_by_address` | C + address | tag |
| `index.v1.state` | C + index name | rules version, height, block hash, running fingerprint |

### 5.2 `arcade.mdbx` — precious, backed up

| Table | Key | Value |
|---|---|---|
| `app.meta` | name | schema version, node created, settings |
| `app.node` | — | node identity key (encrypted), mesh key |
| `app.wallet` | wallet id | encrypted seed, created, next derivation index |
| `app.tag_key` | wallet id + index | public key, address, tag (if claimed) |
| `app.tag_claim` | claim id | tag, key index, txid, status (`checking`/`funding`/`sent`/`confirmed`/`failed`), times |
| `app.faucet_request` | request id | address, peer, amount, txid, status, times |
| `app.peer` | peer id / mesh key | host, port, source (seed/chain), last seen, score, banned |
| `app.bootstrap` | C + kind + height | block hash, fingerprint, size, source peer, status (`downloading`/`verified`/`shared`) |
| `app.event` | time + seq | log of user-visible events for the desktop app |

Secrets (seed, node key) are encrypted at rest with a key kept in a root-only file (`/var/lib/arcade/secret.key`, mode 0600);
no user password, matching the "no password" decision.

### 5.3 Migrations

- `chain.mdbx`: never migrated. A rules change = new table version rebuilt from stored blocks.
- `arcade.mdbx`: `app.meta.schema_version` + an ordered list of Rust migration functions, each run in one write
  transaction, tested against a copy before release. No automatic add-column sweeps.

## 6. Wallet and tags

- First tag: generate a 12-word BIP39 phrase, show it once, require the user to confirm it.
- Each tag gets its own key derived from the phrase (index 0, 1, 2…). Restoring the phrase on any arcade scans
  derived addresses against `ledger.v1.tag_by_address` and recovers every tag.
- Claim flow (steps 6–9): pick tag → validate format → check `ledger.v1.tag` + mempool → request peptest if the
  key's address can't pay the fee → build + sign claim tx → broadcast via P2P and mesh → watch until confirmed.

## 7. Directory layout

```
/usr/bin/arcaded, /usr/bin/arcade           binaries
/etc/systemd/system/arcaded.service         boot service
/var/lib/arcade/chain.mdbx                  chain store (rebuildable)
/var/lib/arcade/arcade.mdbx                 app data (back this up)
/var/lib/arcade/secret.key                  encryption key, 0600
/var/lib/arcade/bootstrap/                  verified snapshots to share
/run/arcade/arcaded.sock                    local API (desktop + CLI)
```

## 8. CLI (step 12)

```
arcade status                 sync height, peers, index state, service health
arcade peers                  list mesh peers
arcade sync                   show/force chain and index sync
arcade bootstrap list|share   snapshots held and shared
arcade wallet create|restore|phrase
arcade tag check <tag>
arcade tag claim <tag>
arcade tag list
arcade faucet                 ask the mesh for peptest
arcade logs
```

## 9. Build milestones

Each ends with a check against something that already exists.

1. **Store + testnet headers/blocks over P2P.** Check: blocks and transactions byte-identical to a known testnet node / existing arcade.
2. **Coins, spends, undo, reorgs.** Check: coin lookups match an existing arcade; forced reorg tests.
3. **Mesh client (existing protocol):** seed list, peers, announcements. Check: shows up as a peer on existing arcades.
4. **Bootstraps:** download, verify, import; then serve. Check: fresh install syncs from mesh bootstrap.
5. **Ledger port (tags, tokens, balances).** Check: per-block fingerprints identical to existing arcades from start to tip.
6. **Wallet + faucet + tag claim.** Check: claim a real tag on testnet; existing arcades see it.
7. **arcaded service + CLI.** Check: survives reboot, resumes cleanly.
8. **Tauri desktop app** for steps 1–9.
9. **Installer** (.deb for Mint/Ubuntu).

## 10. Still to get from the existing arcade (needed before milestones 3–6)

To stay compatible with the current mesh, these specs must be pulled from the existing arcade code (via the Arcade v1.0 / Arcade fix sessions):

- Mesh protocol: transport, handshake, message formats, announcements (`mesh_announcement` payload).
- Built-in seed list: current arcade addresses.
- Bootstrap formats: `bootstrap/*.sqlite.gz`, `*.snap` + `.snap.json`, how peers request and verify them.
  (V1.0 reads SQLite snapshots only as an **import format**, converting them into libmdbx.)
- `index_digest` algorithm and the ledger's per-block fingerprint.
- Ledger rules for tag claims: payload format, validity rules, fee requirements.
- Faucet/top-off protocol: how one arcade asks another for coins, limits, anti-abuse rules.
- Pepecoin testnet parameters: P2P magic (`fec1dbcc`), port 44874, genesis `f9f4ea4a…fc52`, address versions, AuxPoW rules.
- Account identity: today accounts are Ed25519 pubkeys while tags bind to coin addresses — confirm how the two relate so V1.0 derives both from one phrase.

## 11. Prerequisites on this PC

- `git` (not installed: `sudo apt install git`)
- Rust toolchain (`rustup`), plus build tools for libmdbx (`build-essential`, `clang`)
- Tauri dependencies (`libwebkit2gtk-4.1-dev` and friends)

## Shared storage (Martin's lanes, 2026-11-07)

A fifth storage mode, **Shared**, chosen per chain in Chains & Coins (`arcade chain add <key> --mode shared`).
- Everyone keeps the last 10,000 blocks. The history before that is held exactly **3 times**, in lanes A, B, C;
  each lane is one complete copy. Arcades that want more choose **Full** (a complete copy of their own); they count
  as holders but take no lane place.
- In a lane every arcade has a position on the history line (a circle, so a stretch can wrap) and holds from halfway
  to its left neighbour to halfway to its right one. A newcomer joins the lane with the fewest arcades, in the middle
  of that lane's longest stretch, and fetches outward until it meets its neighbours. An arcade that goes leaves an
  orphan stretch that both neighbours restore, meeting in the middle. A lane's first arcade starts at lane/3 of the
  line (offset markers); positions are nudged off the other lanes' edges so no two lanes break at the same block.
  More arcades = smaller shares, never more than 3 copies.
- Blocks are dropped only when at least 3 other arcades announce holding them (in announcements signed by each of
  them; unsigned ones from older arcades are shown but never counted), and only after every index on the chain
  has walked past them. A dropped block keeps header, txids, coins and address history; an index added later fetches
  the bodies it needs back from the holders and drops them again afterwards.
- Ord records split in 64 groups by inscription number (the location map stays whole); PRC-20/DRC-20 ticks in 64
  groups by ticker. Each arcade keeps the least-held groups at install.
- Mesh: `sh hold` (what each arcade holds, its lane and position), `sh q/a` (records from holders); blocks over `bk`,
  checked against our own headers; the coin's P2P network is the backstop.
- Added chains wait for the mesh's offers (V1.0 `bk`, existing arcades' Core nodes over `cr`) before using P2P.
