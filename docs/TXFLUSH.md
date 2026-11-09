# Faster coin-cache flushes: the tx index while catching up — PLAN

Status: **plan for Martin's approval; nothing built** (a read-only counter, examples/txlive.rs, for step 1). Written 2026-11-09 on the GX10 from the JB20B session's findings.

## What the JB20B saw

Dogecoin catching up on the mini PC (main eba674f, `ARCADE_COIN_CACHE_MB=1024`) stops for 150–250 s every ~10 minutes. These are coin-cache flushes, about 25% of the wall time. For example, at 5,278,702:

    flush parts: 4324693 coins 11.5s · 4542275 tx index rows (618214 put off) 216.6s · 320292 older coins deleted 18.2s
    coin cache flushed at 5278702 in 248.1s

The coins go in at ~375,000 rows/s. The tx index (`core.v1.txs`, txid prefix → block and position) goes in at only ~21,000 rows/s. Memory is fine (1.5 GB anonymous; the rest is the database's file map).

The JB20B (reported by its session):
- **RAM:** 15.4 GB, 4 CPU threads; Lexar NM620 2 TB NVMe.
- **Database:** `chain.mdbx` is 446 GB (all chains, Dogecoin at ~5.28 M of 6.45 M); 1.3 TB free.
- **Memory use:** the arcade uses ~1.5 GB of its own memory. The page cache holds ~11 GB, ~7.7 GB of it the database's file map.

So the tx index is far bigger than memory (the database is ~30 × RAM). Random tx-index writes go to disk, which fits the measured ~21,000 rows/s.

## Why the tx index is slow, and what can't be changed

- **Option A is already in.** `flush_cache` sorts the tx rows before writing them (`txs.sort_unstable()`). But txids are random, so a flush's sorted rows still land all over a tree of hundreds of millions of rows. Almost every row touches a different leaf page that isn't in memory. That's random disk I/O, and sorting can't fix it.
- **Most rows can't simply wait (option B on its own).**
  - When a later block spends a coin, its input names the coin by `txid:vout`. We find the coin's row (`outputs`, keyed by block and position) through the tx index. So a tx whose outputs aren't all spent must stay findable right away.
  - Only txs fully spent within the cache can wait. Those are already logged and indexed after the catch-up in one sorted pass: the "618,214 put off".
  - The other ~3.9 M rows per flush are txs that still have coins.

## Step 1 measured (JB20B, Dogecoin at 5,343,559, read-only, 907 s)

| | Rows |
|---|---|
| tx index rows | 325,323,534 |
| unspent coins | 187,807,436 |
| txs with an unspent coin | 168,533,179 (**51.8%**) |

(Pepecoin testnet: 97.7%, mostly unspent coinbases.)

So a "live" tx index would still be ~168 M rows, far bigger than memory, with the same random writes. At best it about halves the cost. **Dropped as the main idea.**

## Why every row is expensive
- The index has ~325 M rows in roughly 3 M leaf pages. A flush brings ~4.5 M sorted rows, so almost every row lands on a different page.
- Each write copies that 4 KB page (copy-on-write) and writes it back: about 4 KB of random disk I/O per 20-byte row.
- **A bigger cache barely helps.** Rows only start sharing pages when a flush has many more rows than the tree has pages.

## The plan: write the tx index sequentially while catching up (sorted levels)

The standard fix for many random inserts into a big index (an "LSM tree"):

1. **Each flush writes its tx rows as a new sorted run**, appended in key order to its own small table (`core.v1.txs_run.<n>`, written with `append`). That is sequential: one page per ~200 rows instead of one per row.
2. **Runs are merged in levels** (like 2048-way sorting in rounds). When ~8 runs of a size pile up, they are merged into one run of the next level, in one sequential pass. Every row is rewritten ~log₈(325 M / 4.5 M) ≈ 2–3 times, always sequentially. Merges run in the background between flushes.
3. **Finding a tx** (a spend's lookup) checks the cache, then the runs from newest to oldest. Each run has a **filter in memory** (a Bloom filter, ~10 bits a row: ~400 MB for all 325 M rows). The filter says "not here" for almost every run, so a lookup touches about one run on disk, the same as now.
4. **At the end of the catch-up**, the levels are merged once into `core.v1.txs` (sequential), the runs and filters are dropped, and the follower writes `txs` directly again as it does at the tip now.
5. **Crash safety:**
   - a run is written and recorded in the same transaction as its flush;
   - a merge writes its new run, then deletes the old ones in one commit;
   - filters are rebuilt from the runs at start.
6. **Other uses of the index while catching up** (explorer lookups by txid) go through the same lookup and keep working.

**Expected:** a flush's tx-index part goes from ~217 s to the time it takes to write ~100 MB sequentially plus a share of the merges. That's seconds rather than minutes, so the ~25% of sync time spent flushing mostly goes away. Still to be measured.

### Steps (each a PR)
1. **Prototype and measure** on the GX10 without touching the JB20B. Sync Pepecoin mainnet (real use, millions of txs) from zero twice, once with today's index and once with sorted runs, with `ARCADE_SYNC_TIMES=1`, and compare flush times.
2. **The final build:** the result must equal a normal sync byte for byte. A V1.0 copy id comparison of blocks, coins and (new) the tx index shows it.
3. **On the JB20B** only with Martin's word: a build with this, for the rest of Dogecoin's catch-up.

### Checks
- **Unit tests:** a lookup finds every tx through any mix of runs and levels; duplicate txids keep the newest (BIP30), as `locate` does now.
- **Crash test:** kill during a flush, a merge and the final build; restart finds the same rows.
- **Equality:** a chain caught up with runs and one caught up the old way give the same `txs` and `outputs`.

## Not touched without Martin's word
The JB20B's running arcaded and its `var/`. Anything measured there is read-only, and only when Martin says so.

## Open question for Martin
1. Build the prototype (step 1, GX10 only), or wait until the mini PC has finished Dogecoin?
