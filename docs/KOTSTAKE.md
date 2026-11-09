# Kotia proof of stake in V1.0: checking and staking — PLAN (not built)

Status: **plan for Martin's approval.** Nothing here is built yet. Written 2026-11-09 on the GX10.

## Martin's decisions

- Kotia is **not installed by default**. It stays an option in Chains & Coins (`arcade chain add kot`), like every other chain. That is how V1.0 already works: only the home chain (Pepecoin testnet) is set up by itself, and the installer and first-run setup install nothing else.
- Staking is a **second, separate option** on top of an installed Kotia. It is off unless the operator turns it on.

## 1. How Kotia's proof of stake works

Source: Kotia Core v2.4 (`eleonoraandrea/kotiacash`, released 2026-09-16): `src/pos.cpp`, `src/main.cpp`, `src/pow.cpp`, `src/wallet/wallet.cpp`, `src/chainparams.cpp`.

**Before and after block 1,000,000:**
- **Up to 1,000,000** (`nLastPOWBlock`): blocks were mined with scrypt proof of work.
- **After 1,000,000:** every block is made by a coin holder, using Blackcoin-style "kernel protocol v3" (protocol V2 and V3 have both been on since 2021, `nProtocolV2Time`/`V3Time` 1635954625/6).

**The lottery (`CheckStakeKernelHash`).** A coin (txid:n, value V, its transaction's time T) wins for timestamp t when

    sha256d(stake modifier || T || txid || n || t) < target(nBits) × V

- **Weight:** V is in satoshis (koinu), so a coin's chance is proportional to its value.
- **Timestamps:** t must be a multiple of 16 s (`nStakeTimestampMask` 0xf), so a staker has one try per coin every 16 s.
- **Maturity:** the coin must be at least 20 blocks deep (`nCoinbaseMaturity`; v3 checks depth, so the 12 h `nStakeMinAge` setting isn't used).

**Stake modifier (`ComputeStakeModifier`, main.cpp:2433).** Every block's modifier is `sha256d(kernel || previous block's modifier)`.
- **For a stake block,** `kernel` is the txid its coinstake spends (vtx[1].vin[0].prevout.hash).
- **For a work block,** `kernel` is the block's own hash.
- **At genesis** the modifier is 0.

So the modifier is a hash chain over **every block since genesis**, and checking or staking needs the whole chain.

**A stake block (`CheckBlock`, `ConnectBlock`, main.cpp 3592–3611, 2619–2624):**
- **Coinbase:** tx 0 has exactly one, empty output.
- **Coinstake:** tx 1 is the coinstake: input 0 is the winning coin, output 0 is empty, and the outputs after it pay the staker. No other transaction may be a coinstake.
- **Time:** the block time equals the coinstake's nTime and is a multiple of 16. It must be after the previous block's time (`GetPastTimeLimit`, protocol V2).
- **Reward:** the coinstake may pay out at most its inputs + **20 KOT** (`GetProofOfStakeSubsidy`) + the block's fees.
- **Block signature** (`vchBlockSig`): an ECDSA signature over the block hash by the key in the coinstake's **output 1** (a pay-to-pubkey script, or a key pushed in a non-spendable output). Kotia's own wallet turns a pay-to-address kernel into a pay-to-pubkey output for this (wallet.cpp 756–782).
- **Difficulty** (`CalculateNextTargetRequired`, pow.cpp): uses the last two stake blocks.
  - Target spacing is 120 s and the timespan 600 s, so the interval is 5.
  - new = old × (4×120 + 2×actual) / (6×120), with actual capped at 10 × 120 s.
  - It is limited by `posLimit` (00000fff…).
- **Weight for choosing the best chain:** the usual work from nBits, the same as V1.0's `work_from_bits`. This was already checked.

**How Kotia's wallet stakes (`CreateCoinStake`):**
- **Search:** every few seconds it tries each mature coin over the newest timestamps (up to 60 s back).
- **On a win:** it adds smaller coins of the same script to the coinstake (combine, coins under **500 KOT**), and splits the output in two when it exceeds **1,000 KOT** (`GetStakeSplitThreshold`).
- **Reserve:** a `reserveBalance` setting keeps part of the balance out of staking.

## 2. What V1.0 does today

- **Following and syncing:** follows Kotia's proof-of-stake blocks and checks their structure, transactions, merkle root and linkage. A full sync from block 0 to the tip (1,538,993) worked on 2026-11-09, with the tip hash matching other arcades' Kotia Cores.
- **Not checked:** **the stake itself.** The lottery, the modifier, the coinstake rules, the block signature and the stake difficulty are taken on trust from Kotia's nodes (`check_pow_or_stake` → "PoS block: not checkable").
- **Making blocks:** none.

## 3. Phases (each one a PR)

### Phase 1: check proof of stake (full mode only)
- **A new table `pos.v1.modifier`** (C|height → 32-byte modifier), written for every block in the same commit as the block, with undo for reorgs. Kotia installed in full or chain mode has every block, so it can be computed from genesis. On an existing install it backfills once in a pass.
- **Check every stake block** before connecting it:
  1. tx 0 has one empty output, tx 1 is the only coinstake;
  2. the time rules;
  3. the kernel coin is at least 20 deep;
  4. the kernel hash is under target × value;
  5. the coinstake reward is within 20 KOT + fees;
  6. the block signature is right, by the key of output 1;
  7. nBits equals the stake difficulty computed from the last two stake blocks.

  A failure refuses the block, as a bad proof of work does now.
- **Borrowed Kotia** (no blocks stored) keeps the trust model it has now.
- **Test:** re-check all 538,993 stake blocks of the real chain. Every one must pass, and a few hand-broken copies must fail.

### Phase 2: the staker (an option per arcade, off by default)
- **Where:** Chains & Coins → Kotia → "Stake my KOT" (Admin; `chains.stake {key, on, reserve?}`). It needs Kotia installed in full or chain mode and synced.
- **Coins it stakes:** the wallet's own KOT coins (tag keys; imported WIF keys if the operator includes them), 20 or more blocks deep, less the reserve.
- **Every 16 s slot,** while synced to the tip, it tries each coin. On a win it builds:
  - **the coinstake:** the kernel coin, plus small coins of the same address up to 500 KOT each (Core's combine rule). Output 0 is empty. Output 1 goes to the key's pay-to-pubkey (split in two over 1,000 KOT). The value is inputs + 20 KOT + fees;
  - **the block:** the coinbase (BIP34-style height and an empty output), the coinstake, and waiting transactions that fit, to earn their fees. A first version can leave them out;
  - **the signatures:** both transactions signed (legacy SIGHASH_ALL with nTime), and the block signed with the stake key (low-S DER);
  - **checks before sending:** the block is checked with the phase 1 checks, connected here, and sent to Kotia's P2P peers.
- **Status:**
  - the stake weight (coins taking part), the network's estimated stake weight, the expected time to a reward, and rewards found;
  - stake rewards show in the Wallet as "staking, matures in N blocks" (20).
- **Safety:**
  - **Hot keys:** staking needs the wallet's keys on the machine all the time. They already are, for the arcade wallet.
  - **Lost blocks:** a block that loses a race only costs that reward, never coins.
  - **Several machines:** two machines staking the same coins don't lose coins either; they would only orphan each other's blocks. The option should say "stake this wallet on one machine".
- **Tests:**
  1. **Kernel replay:** for past stake blocks, recompute the winning kernel and coinstake from the block's own coin and time, and rebuild the coinstake's shape.
  2. **Simulation:** a dry run over recent history with a test wallet's coins: when would they have won?
  3. **Live:** a small amount of KOT on mainnet, on the mini PC, watching for a found block. This needs Martin's OK and some KOT; Kotia's test network has no public nodes.

### Phase 3: polish
- **Combine:** gather many small coins into one, as Core's combine does, so staking stays cheap.
- **Settings:** reserve and coin-selection settings.
- **History:** a staking history in the Wallet.

## 4. Effort and order

| Phase | Size | Depends on |
|---|---|---|
| 1. Check proof of stake | M–L | Kotia installed (full/chain) |
| 2. Staker | L | Phase 1 (its checks are the staker's own self-test) |
| 3. Polish | S–M | Phase 2 |

Phase 1 is worth doing on its own: it ends V1.0's trust in Kotia's nodes for stake blocks.

## 5. Open questions for Martin
1. **Which machine stakes:** the mini PC (online all the time, Kotia to be installed there after Dogecoin), or another?
2. **Which KOT:** the arcade wallet's own KOT, or imported keys too? And how much for the first live test?
3. **Waiting transactions:** should found blocks include them (more fees, more code), or only the coinbase and coinstake at first?
