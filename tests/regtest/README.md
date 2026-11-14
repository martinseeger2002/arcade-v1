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
| An unsigned spend of a matured coin that needs a signature | script check fails | refuse |
| A correctly signed spend; one signed by the wrong key | takes it; script check fails | the same |
| A transaction locked 500 blocks ahead | bad-txns-nonfinal | refuse |
| 20,001 signature operations; a block over 1,000,000 bytes | bad-blk-sigops; bad-blk-length | refuse |
| After 1,351: a version-3 block | bad-version | refuse |
| After 1,251: a signature that isn't strict DER | script check fails (DERSIG) | refuse |
| After 1,351: a CHECKLOCKTIMEVERIFY coin with no lock time; then with lock time 1,000 | fails; then takes it | the same |

The block builder's own coins pay to `OP_TRUE` (spent with no signature, to try fees, maturity
and amounts exactly) or to a key of its own (signed with Python's `cryptography` package,
legacy SIGHASH_ALL). The run mines to height 1,360, so DERSIG (1,251), CHECKLOCKTIMEVERIFY and
version 4 (1,351) are in force for the last scenarios.

A scenario can be marked `gap=True` for a rule V1 doesn't check yet. The run reports it
without failing. Since step 3 there are none. After each bad block, Core mines two more
blocks, and V1 must follow Core's chain.

On `main` before step 1, the first 16 scenarios gave 6 match, 3 gaps and 7 FAIL. With steps 1
to 3, all 38 match: no gaps.

# The SegWit-era harness: V1 against a real Litecoin Core

`ltcnet.py` does the same against Litecoin Core 0.21 in `-regtest`, for the rules Bitcoin Core
0.13 and later chains add. V1 follows with `regtest_follow ... ltcregtest`.

```sh
cargo build --release -p arcade-chaindb --example regtest_follow
unshare -Urn sh -c 'ip link set lo up; python3 tests/regtest/ltcnet.py'
```

- **What it needs:** `litecoind` (0.21.x) on `PATH` or in `~/.local/bin`, and Python 3 with the
  `cryptography` package. Schnorr signatures (BIP340) are made by a small pure-Python secp256k1.
- **Options:** `--litecoind PATH`, `--follow PATH`, `--keep`, `--only a|b`.
- **Two runs:** A keeps MWEB off (`-vbparams`) and gives both hand-built good and bad blocks. B
  lets MWEB activate and has Core's wallet peg in, move coins inside MWEB and peg out. Once MWEB
  is on, every block needs a real extension block, so bad blocks are built in run A only.
- **Each scenario states Core's verdict** ("both take it", or "Core: <reason>"). If Core's
  answer differs, the scenario is marked BUILD and fails the run: the block wasn't what the
  scenario meant, so it proves nothing about V1.

| Run A scenario | Core | V1 must |
|---|---|---|
| A version-4 block once SegWit is on | bad-version (whole nVersion under 0x20000000) | refuse |
| P2WSH OP_TRUE spend; signed P2WSH spend (BIP143) | takes them | take them |
| Witness spend with no commitment; wrong commitment with and without witness data | unexpected-witness; bad-witness-merkle-match | refuse |
| Witness data on a non-witness spend | script check fails | refuse |
| P2WSH signed over the wrong amount | script check fails | refuse |
| Witness CHECKMULTISIG with a non-empty dummy; then an empty one | NULLDUMMY fails; takes it | the same |
| Taproot key-path spend with a broken signature; then a good one | fails; takes it | the same |
| 90,000 witness signature operations | bad-blk-sigops | refuse |
| A block of weight over 4,000,000 | bad-blk-weight | refuse |
| Version-2 spend with a 10-block relative lock, 5 and then 10 blocks in | bad-txns-nonfinal; takes it | the same |
| The same sequence on a version-1 transaction | takes it | take it |
| 512-second relative lock, at once and then later | bad-txns-nonfinal; takes it | the same |
| Lock time between the median time past and the block's time (BIP113) | bad-txns-nonfinal | refuse |
| `<10> CHECKSEQUENCEVERIFY` spent with sequence 5, then 10 | fails; takes it | the same |
| Coinbase one unit over a halved subsidy; then exact | bad-cb-amount; takes it | the same |

Run B: the first MWEB block (with a peg-in), a transfer inside MWEB (its fee leaves through
the HogEx), a peg-out, its spend 6 blocks later, and five more mixed blocks. V1 must follow
every one. MWEB's own rules (kernels, range proofs, the peg-in and peg-out matching) aren't
checked by V1.

Run A: 26 match. Run B: 5 match.
