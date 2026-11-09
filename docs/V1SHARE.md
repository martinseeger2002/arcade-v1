# Sharing chains and indexes between V1.0 arcades — PLAN

Status: **approved by Martin 2026-11-09 ("write the plan and then start building"); being built phase by phase (§9).** Written on the GX10.

| Phase | State |
|---|---|
| 1. Copy format and makers | building |
| 2 to 6 | not started |

## Martin's decisions

- Once Dogecoin is at the tip on the mini PC, a **new V1.0 install should get it faster** from V1.0 arcades. That covers the chain itself (blocks and coins) and the indexes (ord, ledger, addresses, ...).
- **Several V1.0 arcades share the same copy at the same time**, so it gets faster as there are more V1.0 arcades (torrent-style).
- **Pick the parts you want**, as Chains & Coins already lets you: for example **only Dogecoin's ord index**, without the Dogecoin blocks or coins (§5.1).
- **Every part is optional except the home chain and its index.** The home chain (Pepecoin testnet by default) and the arcade's own index on it (the arcade ledger: @tags, messages, posts, tokens, apps) are always installed. Every other chain, and every part of each one (blocks, coins, addresses, ord, tokens, names, maps, ledger), is picked or left out.
- **Trust** (from INDEXBOOT): use a copy once **2 or more other arcades** agree with it, then rebuild it from our own blocks in the background ("trusted" now, "verified" later).

This replaces INDEXBOOT §2.2, §5 "Native copies" and §7 (phase 5, `vx`), and adds what V1.0 lacked for chains: today V1.0 **takes** chain copies from old arcades (`cb`, from their Core's files) but **makes** none (`meshview.rs`: "V1.0 makes no chain copies").

## 1. The idea: identical copies, made independently

Every V1.0 arcade that shares a chain makes its copies **at the same heights** (checkpoints) and writes them **byte for byte the same way**. Two arcades at the same height hold the same rows, so their copies get the **same manifest id**.

That gives three things at once:
1. **Swarm:** a new install fetches pieces from **every** arcade holding that id, 6 at a time from each. Ten V1.0 arcades → about ten times the speed of one.
2. **Trust for free:** the same id announced by 2 or more arcades that each **made** it from their own rows is the "2 or more agree" check, with no separate digest to compute or ask for.
3. **Re-seeding:** an arcade that took a copy in keeps its pieces and serves them under the same id until its own next checkpoint copy replaces them.

What makes a copy identical everywhere:
- **Checkpoint heights:** multiples of a per-chain interval `K`, about one a day (§3).
- **Canonical rows:** only rows that follow from the chain itself, in key order, with local numbers replaced by chain facts (§2).
- **Fixed encoding:** our own piece writer, pieces of exactly 8 MiB of the row stream (the last one shorter), each compressed on its own with a fixed zstd level and no dictionary, the same crate version pinned. The manifest is canonical JSON.
- **Format version** in the manifest (`fmt`, bumped whenever a family's rows change between releases). Only copies of the same `fmt` share an id.

## 2. Families

| Family | Our tables | Canonical form in the copy |
|---|---|---|
| **blocks** | `core.v2.headers hdr`, `core.v1.height block_meta block_tx txs` | per height: the 80-byte header (+ AuxPoW/PoS extras as stored), then each raw tx. Internal block numbers (`B`) are not exported; the importer gives its own. |
| **coins** | `core.v2.outputs` | every unspent output as `height | tx position | vout | value | flags | script bytes`, in that order. The script id (`SID`) and `B` are local and are replaced by the script bytes and the height. |
| **addresses** | `core.v2.script_hist by_script spends` | `script_hist` as is (keyed by height and txid). `by_script` and `spends` with `B` → height and `SID` → script bytes, as for coins. |
| **ord** (+ riders tokens, names, maps) | `ord.v1.*`, `tok.v1.*`, `names.v1.*`, `maps.v1.*` | as stored (keyed by txids, heights, numbers, names). Local working rows (pending bodies and the like) are left out and rebuilt after import, as after an old-arcade copy. |
| **ledger** | `ledger.v1.*` | as stored. |
| **messages** (public posts, acts, profiles) | `msg.v1.cand post post_tx file gchunk act act_to machine profile linked` | as stored; nothing private (wallets, mailboxes, read marks) is in these tables. |

- **One family, one copy.** A new install can take blocks + coins (the chain) and then the indexes it wants.
- **Not shared:** mempool, undo records, wallet and app data (`arcade.mdbx`), copy and walk progress state.

## 3. Making copies

- **When:** each family's own writer (the chain follower for blocks/coins/addresses, the ord walker, the ledger engine, the messages indexer) commits the checkpoint block on its own. Right after that commit it opens a **read transaction**, a frozen view of that height. It goes on with the next block at once while a background thread writes the copy from the frozen view. Only that chain's keys are read (`C|...`), so other chains writing meanwhile don't matter.
- **Only at the tip:** while catching up the follower writes in batches and passes checkpoints mid-batch, so no copy is made then. An arcade starts making copies once that family is at the tip.
- **Interval `K`** (blocks between checkpoints, about a day): Dogecoin, Pepecoin, Flopcoin 1,440; DigiByte 5,760; Litecoin 576; Bitcoin 144; Kotia 720; others from their block spacing. Indexes that are expensive to scan (ord on Dogecoin) use 7 × K.
- **Settled:** a copy at checkpoint `cp` is offered once the tip is 6 blocks past it, and only while our block at `cp` is still on the best chain.
- **Disk:** one copy per family kept, the previous one deleted when the new one is complete. Not made while free disk would fall under 2 × the copy size. Copies live in `<home>/copies/<net>/<family>/<id>/`.
- **Who makes them:** every arcade with sharing on for that chain (the same "share" switch the chain already has in Chains & Coins, on by default), whether it is a full node or a chain-mode install that has the family.

## 4. Wire: frame `vx`

On direct links and relays. Old arcades ignore an unknown `t`.

- `hi {nets: {net: {fam: {id, cp, hash, bytes, n, made, fmt}}}}`: the copies this arcade holds, with `made: true` when it made the copy from its own rows, false when it is re-seeding one it took in. Sent on link-up and whenever a copy is added; kept 10 minutes by the receiver.
- `g {id, net, fam, snap, what: "manifest"|"piece", i}` → `gb {id, seq, end[, err]}` plus 60 KiB slices, as `cb` does. `err` is `busy` (rate limit: pause that holder 10 s) or `gone`.
- **Limits per server:** 6 pieces served at once, 240 pieces a minute per asker. Manifest requests are free.
- **Fetching:** 4 pieces in flight per holder, across every holder of the id. A piece is lost after 120 s, a holder silent for 900 s is dropped. Each piece is checked against the manifest's sha256 before it is kept. The bitmap of pieces held is saved, so a fetch continues after a restart.

Manifest (canonical JSON, id = sha256 of it):
`{v: 1, fmt, net, genesis, fam, cp, hash: <block hash at cp>, rows, bytes, piece: 8388608, pieces: [sha256 of each compressed piece], raw: [length of each raw piece], digest: <sha256 of the whole raw row stream>}`.

## 5. Taking a copy in

**When:**
- The chain is not installed, or a family is 20,000 blocks or more behind, **and**
- a copy is offered whose id at least **2 makers** announce (`made: true`, different node ids).
- For the indexes, the chain's blocks must already be here up to `cp`, as INDEXBOOT §4.1 says. For blocks and coins they come in with the copy.

**Order for a new install (`arcade chain add doge` with sharing):**
1. Headers from the network or arcades, checked by proof of work as always. The copy's `hash` at `cp` must be our own best header at `cp`.
2. **blocks** copy: fetched from the swarm and written in height order. Each block is checked against our header (merkle root), which needs no coins. Fast: no scripts, no coin lookups.
3. **coins** copy: written into `core.v2.outputs` (our own `B` and `SID`). The coin count and total must equal the manifest, and the digest of the raw stream must match.
4. Marked **trusted from <makers>**. The chain follows the network from `cp` on as usual.
5. Then any index copies the operator chose (addresses, ord, ledger, messages), the same way.

Each step is resumable and can be cancelled. A failed check refuses that id, and its makers are named in the events.

### 5.1 Only the parts you pick

Every family is its own copy, so an install takes only what was picked in Chains & Coins. The **home chain** is the exception: its blocks, coins and arcade index are always taken (they are what an arcade is), and only its other parts are optional.

| Picked | Taken from copies | Kept current by |
|---|---|---|
| Full (chain + indexes) | blocks, coins, then the indexes picked | following the network, as now |
| Chain only | blocks, coins (+ addresses if picked) | following the network, as now |
| **Index only**, e.g. just Dogecoin ord | headers from the network, then **only that index's copy** (no blocks, no coins) | each new block fetched from arcades holding the chain, together with **the outputs it spends** (which an index like ord needs for its moves, and which a chain holder has in its undo records). The block is checked against our headers; the spent outputs are taken when 2 holders give the same ones. |

- **Index only** is the mode Chains & Coins already lists as "coming" (`chains.rs` refuses `index` today). This plan builds it: `arcade chain add doge --mode index --with ord`.
- An index-only install holds **no blocks**. A lookup that needs one (an inscription's full body, a transaction page) asks the arcades holding the chain, as a borrowed chain does now.
- More parts can be added later. Picking the chain later takes the blocks and coins copies; the index is kept.
- Index-only installs **re-seed** the index copy they took, and make their own at the next checkpoint (§3), so they add to the swarm like everyone else.

## 6. Background re-check ("verified")

- **Coins:** derived again from our own blocks, as after a `cb` copy (`cbimport` step 3). Equal → verified; different → ours replace the copy's, and the makers are named.
- **Blocks:** every script checked in the background (the part skipped on import), block by block from genesis. Any failure → the chain is marked bad from there and synced again from the network.
- **Indexes:** rebuilt into a shadow file from our own blocks and compared at the copy's `cp` (INDEXBOOT §6).
- All of this runs only while the chain is at the tip and nothing else is fetched, at low priority, and can be paused.

## 7. UI and CLI

- **Chains & Coins**, per chain, one line per family:
  - "blocks: copy at 6,450,240 · held by 4 arcades (3 made it) · 62% · 41 MB/s";
  - "ord: trusted (3 arcades) · verifying 12%".
- **Home mesh overview:** V1.0 arcades show "chain copy · block N" and "ord copy · block N", the way old arcades do now.
- **CLI:**
  - `arcade chain copy status [key]`
  - `arcade chain copy make <key> <fam>` (now, at the last checkpoint, for testing)
  - `arcade chain copy take <key> <fam>`
  - `arcade chain copy cancel <key> <fam>`
  - `arcade chain copy share <key> on|off`
- **API:** `chains.v1copy_status|make|take|cancel|share` (admin, except status).

## 8. Testing

1. **Identical copies:** the GX10 and the mini PC each make a Pepecoin testnet copy of every family at the same checkpoint. The ids must be equal. Any difference is found row by row and fixed (local fields leaking into a family).
2. **Swarm:** a throwaway V1.0 home takes Pepecoin testnet blocks + coins + ord from both at once. The pieces must come from both, and the result must equal the makers' tables.
3. **Trust:** a copy announced by 1 maker is not taken; a tampered piece is refused; a copy whose `hash` isn't on our best chain is refused.
4. **Dogecoin:** once the mini PC is at the tip with ord, a third machine installs Dogecoin from it, with time and speed measured. The GX10 then makes its own copies, and the next install pulls from both.

## 9. Phases (each one a PR)

1. **Copy format and makers:** canonical row export per family, the piece writer, the manifest, checkpoint copies, `arcade chain copy make`. Tested by comparing the ids of independent machines (§8.1).
2. **Serving and the swarm fetch:** `vx` hi/g/gb, limits, resumable fetching from many holders, re-seeding.
3. **Chain install from copies:** headers → blocks → coins, trusted, then following the network. The coin re-check from `cbimport` is reused.
4. **Index copies:** addresses, ord (+ riders), ledger, messages, and **Index-only installs** (§5.1): a block with its spent outputs from chain holders (a new `bk` question), and an index following the tip from those.
5. **Background script check of copied blocks**, and the shadow re-check for indexes.
6. **UI:** Chains & Coins lines and the Home overview.

## 10. Open questions for Martin

1. **Sharing default:** on for every installed chain (as the "share" switch is now), or asked per chain?
2. **Disk:** each arcade keeps one copy per family. For Dogecoin that is roughly the size of the chain's tables again (blocks + ord ~ the most). Is that OK on the mini PC and the GX10, or should big families be made only on request?
