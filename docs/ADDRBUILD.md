# Address build: a progress bar per step, and resuming after a restart — PLAN

Status: **plan for Martin's approval; nothing built.** Written 2026-11-10 after the JB20B's Dogecoin catch-up.

## What the JB20B showed
- **One row for six steps:** the address build after a catch-up has six steps, but chain status shows one row, "Addresses · build", at 0.0% and 0.0 blk/s while step 1 sorts ~150,000 rows/s.
- **No resume:** a restart started step 1 over (204.2 M rows, block 2,020,782 of 6,443,539, back to 661k). That cost ~20 minutes.

## The six steps (crates/arcade-chaindb/src/addrbuild.rs)
| Step | What | Measured in |
|---|---|---|
| 1 | Sort the log into run files (the catch-up's logged rows) | blocks of the log read, of the build's height |
| 2 | Address history, written in key order | rows written, of the rows sorted for it |
| 3 | Spends | rows, as above |
| 4 | Address coins | rows, as above |
| 5 | Transaction index | rows, as above |
| 6 | Clear the log | log rows deleted, of those read in step 1 |

A chain added with Addresses on reads its stored blocks in step 1 instead (backfill). That shows as the same step 1, in blocks.

## 1. A bar per step
- **chaindb reports numbers, not text:** `build_addresses` reports `{step, of: 6, name, done, total, unit: "blocks"|"rows"}` through its progress callback instead of a sentence.
  - Steps 2–5 know their totals from step 1, which counts the rows per kind.
  - Step 6 knows the log's row count.
- **arcaded** keeps the latest report per chain and adds a rate per step (the existing `stage_rate` sampling: blk/s or rows/s).
  - `chains.status` lists six stages ("Addresses · 1 sort the log" … "Addresses · 6 clear the log"), each with `pct`, `rate` and `unit`.
  - Finished steps are at 100%, waiting ones at 0%.
- **The desktop UI** (Chains & Coins) draws six bars, using the same stage list.
- **The CLI** (`arcade chain status <key>`) prints six lines.

## 2. Resuming step 1
- **Recorded as it goes:** each run file step 1 writes is recorded in STATE `addr_runs` (file name, kind, rows) with the log key it got to, in the same write that notes progress. The files live in `<home>/tmp/addr-<chain>/` and are only deleted at the end of step 6.
- **On restart:** with `addr_runs` present and its files there, step 1 goes on from the recorded log key with the files it has; if a file is missing, it starts over.
- **Steps 2–5:** each writes its rows with `put` (doing it again changes nothing). A finished step is recorded in `addr_runs` too, so a restart skips the steps already done and redoes only the one it was in.

## Checks
- **Unit tests:**
  - stop the build in step 1 and in step 3, restart, and the address tables equal those of an uninterrupted build;
  - the progress reports go 0 → 100% for each step.
- **On the JB20B:** watch the six bars on Dogecoin's next build, which only happens after another catch-up.

## Size
Small to medium: a progress struct and STATE records in addrbuild.rs, the stage rows in chains.rs, the bars in chains.js, and lines in the CLI.
