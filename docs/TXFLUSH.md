# Faster coin-cache flushes: the tx index while catching up — PLAN

Status: **plan for Martin's approval; nothing built.** Written 2026-11-09 on the GX10 from the JB20B session's findings.

## What the JB20B saw

Dogecoin catching up on the mini PC (main eba674f, `ARCADE_COIN_CACHE_MB=1024`) stops for 150–250 s every ~10 minutes. These are coin-cache flushes, about 25% of the wall time. For example, at 5,278,702:

    flush parts: 4324693 coins 11.5s · 4542275 tx index rows (618214 put off) 216.6s · 320292 older coins deleted 18.2s
    coin cache flushed at 5278702 in 248.1s

The coins go in at ~375,000 rows/s. The tx index (`core.v1.txs`, txid prefix → block and position) goes in at only ~21,000 rows/s. Memory is fine (1.5 GB anonymous; the rest is the database's file map).

## Why the tx index is slow, and what can't be changed

- **Option A is already in.** `flush_cache` sorts the tx rows before writing them (`txs.sort_unstable()`). But txids are random, so a flush's sorted rows still land all over a tree of hundreds of millions of rows. Almost every row touches a different leaf page that isn't in memory. That's random disk I/O, and sorting can't fix it.
- **Most rows can't simply wait (option B on its own).**
  - When a later block spends a coin, its input names the coin by `txid:vout`. We find the coin's row (`outputs`, keyed by block and position) through the tx index. So a tx whose outputs aren't all spent must stay findable right away.
  - Only txs fully spent within the cache can wait. Those are already logged and indexed after the catch-up in one sorted pass: the "618,214 put off".
  - The other ~3.9 M rows per flush are txs that still have coins.

## The plan

### 1. Measure first (read-only)
On a copy of a Dogecoin database (or on the JB20B, read-only, with Martin's OK):
- **How many txs still have unspent outputs** (distinct block+position in `outputs`) compared with all tx index rows. That is the size of the "live" index below.
- **Page cache:** the machine's RAM against the tx index's size, to confirm it's random I/O. Read faults during a flush show it.

### 2. A small "live" tx index while catching up
- **New table** `core.v1.live_txs` (txid prefix → block and position), the same rows as `core.v1.txs` but only for **txs that still have an unspent coin**.
- **While catching up** (more than `DEFER_BEHIND` = 20,000 blocks behind, as addresses already are):
  - a flush writes new txs' rows into `live_txs` instead of `txs`, and logs every row (`TXROW` in `addr_log`, as the fully spent ones are now) for the final sorted build;
  - a tx whose last coin a flush deletes leaves `live_txs`. The flush already deletes those coins in key order, and whether any coin of that tx is left is a cheap look at the dense `outputs` rows next to it.
- **Finding a tx** (`locate`): the cache, then `live_txs`, then `txs`.
- Why it's faster: `live_txs` holds only txs with unspent coins. That is far fewer rows than the whole history (step 1 gives the number), so many more of its pages stay in memory.

### 3. One sorted build at the end
- When the chain gets within 20,000 blocks of the tip, the logged rows are sorted in runs and written to `txs` in key order, as `addrbuild` does now. Pages are written once, in order.
- Then `live_txs` is emptied and the follower goes back to writing `txs` directly.
- Until then, **lookups of old fully spent txs** by txid (explorer pages) aren't possible, the same as addresses during a catch-up now.

### 4. Checks
- **Unit test:** a chain caught up with the live index and one caught up the old way get the same `txs` and `outputs` rows, byte for byte. The V1SHARE copies make this easy to compare: equal copy ids.
- **Crash test:** kill during a flush and during the final build; on restart the tip winds back to `coins_at` and the build resumes.
- **Timing:** the same Dogecoin stretch with `ARCADE_SYNC_TIMES=1` before and after, comparing flush parts.

## Not touched without Martin's word
The JB20B's running arcaded and its `var/`. Anything measured there is read-only, and only when Martin says so.

## Open question for Martin
1. Measure on the JB20B's live Dogecoin database (read-only: a count of rows, no writes), or only on a copy elsewhere?
