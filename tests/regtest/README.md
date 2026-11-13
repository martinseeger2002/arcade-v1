# Consensus harness: V1 against a real Pepecoin Core

`regnet.py` runs Pepecoin Core in `-regtest` and gives V1 the same blocks. Core mines the good
ones. The script builds the rest by hand: bad blocks and competing branches, with real scrypt
proof of work and real AuxPoW. Every block goes to Core (`submitblock`) and to V1. V1 reads them
through `examples/regtest_follow`, which runs the real `Syncer` with every check it makes on a
live chain. After each scenario, Core's best block and V1's tip must be the same block.

## Running it

```sh
cargo build --release -p arcade-chaindb --example regtest_follow
unshare -Urn sh -c 'ip link set lo up; python3 tests/regtest/regnet.py'
```

- **What it needs:** `pepecoind` (Pepecoin Core 1.1.0) on `PATH` or in `~/.local/bin`, and
  Python 3 with nothing extra installed.
- **Other builds:** `--follow PATH` picks another `regtest_follow` build (for example, one from
  an older commit, to see what a change fixed).
- **Keeping the files:** `--keep` keeps the work directory (Core's data, V1's store and the
  block file).
- **Exit code:** nonzero when a scenario that must match doesn't.

## Scenarios

| Scenario | Core | V1 must |
|---|---|---|
| Core mines blocks, before and after merge mining starts (height 20) | follows | follow |
| A merge-mined block before height 20 | early-auxpow-block | refuse |
| A version-1 (legacy) block after height 20 | late-legacy-block | refuse |
| A merge-mined block carrying chain id 64 instead of 63 | refuses (strict chain id) | refuse |
| A hand-built merge-mined block | takes it | take it |
| Witness data on a chain without SegWit | unexpected-witness | refuse |
| A heavier branch whose 2nd block spends a coin that doesn't exist | marks it invalid, stays | mark it, stay |
| A valid heavier branch | reorgs | reorg |
| A coinbase paying 10,000,000 PEP; the subsidy plus the fee plus 1 unit | bad-cb-amount | refuse |
| The subsidy plus exactly the fee | takes it | take it |
| Two coinbases; a 1-byte coinbase script | bad-cb-multiple; bad-cb-length | refuse |
| Spending a coinbase 59 blocks later; then at 60 | premature-spend; then takes it | the same |
| Paying out 1,001 PEP from a 1,000 PEP coin | bad-txns-in-belowout | refuse |
| A mutated copy of a block, then the real block | refuses the copy, takes the block | the same |
| An unsigned spend of a matured coin that needs a signature | script check fails | *gap: step 3* |

The block builder's own coins pay to `OP_TRUE`, so Core takes spends of them without a
signature. That lets fees, maturity and amounts be tried exactly.

Rows marked *gap* are rules V1 doesn't check yet: scripts and signatures, which are step 3 of
the plan. The run reports them without failing. After each gap, Core mines two more blocks,
and V1 must come back to Core's chain.

When a later step adds a rule, drop `gap=True` from its scenario.

On `main` before step 1, the first 16 scenarios gave 6 match, 3 gaps and 7 FAIL. With steps 1
and 2, all 26 give 25 match, 1 gap and 0 FAIL.
