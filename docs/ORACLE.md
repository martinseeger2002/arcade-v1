# The hosting oracle: valuing ARCADE by what it costs to run an arcade — PLAN

Status: plan, nothing built. Martin, 2026-11-16: "the [relay] VPS will accept cryptocurrencies as payment, some of
the cryptocurrencies that we currently support; that means we can value the ARCADE coin using an oracle linked to the
cost to host an arcade node, including larger VPSs and VPSs with graphics cards: a fully scalable, self-rewarding,
sustainable system."

## Goal

An arcade's operator should be able to pay for the machine it runs on out of what the arcade earns, at any size:
- a small VPS that relays and keeps a few chains;
- a large one that keeps every chain in full;
- one with graphics cards that runs compute jobs (the LLM service, rendering, media).

For that, the network needs to know two prices it doesn't know today:
1. **What hosting costs**, in a coin the arcades already follow (DOGE, LTC, PEP, DGB, ...). Hosts that take crypto
   publish it, and operators pay it on chain.
2. **What ARCADE is worth in those coins.** The market's atomic swaps (docs/MARKET.md) settle on chains the arcades
   already check.

Together they give **ARCADE per hosting unit**. Fees and rewards set in hosting units then keep their real value as
ARCADE's price moves, so the network keeps paying its hosts as it grows.

## What exists to build on

- **Earning by work** (docs/EARN.md): storage weight and the pot, paid once per audit period after passed audits.
- **Relayed links with a usage fee** (v0.1.95, v0.1.96): a relay's operator sets a price per GB and a free allowance.
  Arcades pay in ARCADE, with a memo, credited once in a block. The price is a fixed number the operator types.
- **Atomic swaps** between ARCADE and other coins (docs/MARKET.md). Their lock and claim transactions are on chains
  every arcade follows the headers of (`registry::Sources`), so a swap can be proven with a Merkle path, the way
  fraud proofs are (§2.4).
- **Source chains' headers** on every ARCADE node: anything that happened on DOGE, LTC, PEP and the rest can be
  checked by the ARCADE chain without trusting anyone.
- **The LLM service**: per-model credits on the original arcades. It's the first compute job with a price.

## Hosting units

A **hosting unit (HU)** is one month of a reference machine, in three tiers, each a basket of public offers:

| Tier | Reference machine | What it does on the network |
|---|---|---|
| HU-S | 2 vCPU, 4 GB RAM, 80 GB disk, 2 TB traffic | relay, light chains, the ARCADE chain |
| HU-L | 8 vCPU, 32 GB RAM, 2 TB NVMe, 10 TB traffic | every chain in full, indexes, storage weight |
| HU-G | HU-L plus one 24 GB GPU | compute jobs |

Smaller and larger work is priced in fractions and multiples. For example, a relay's GB is 1/2000 of an HU-S's
traffic, and a GB of storage for a month is 1/2000 of an HU-L's disk. The reference specs live in the rules: a rules
version changes them, as hardware gets cheaper.

## The two prices

### ARCADE in coins: from swaps on chain

- **What counts:** a settled ARCADE↔coin swap is a pair of transactions, the lock and claim on ARCADE and on the
  other chain. Anyone can post a **swap report** to the ARCADE chain: the two transactions with Merkle paths to
  headers 6+ deep. The rules check that both sides match the HTLC shape and the same hash.
- **The index:** for each epoch (100 blocks), the volume-weighted median of the epoch's reported swaps, per coin. It
  moves at most ×1.25 per epoch, like the difficulty clamp, and stays as it was in an epoch with too little volume.
- **Wash trading** (swapping with yourself to move the price):
  - the median, not the mean;
  - counted only between distinct keys that each paid a normal fee on both chains;
  - no more than a set share of the epoch's volume per key;
  - the clamp.

  Moving the index takes real volume at a fake price, every epoch, against everyone else's real trades.

### Hosting in coins: from reports with evidence

- **Quotes:** an operator posts a **hosting quote**: the tier, the provider's public price in a coin, and the hash of
  the offer page. Each quote is signed by an arcade with weight.
- **Payments:** a **hosting payment** is stronger: the payment transaction itself, proven on its source chain, plus
  the tier. Payments count double, because they cost real money.
- **The index:** the median of the last 90 days of quotes and payments per tier and coin, again with a clamp per
  epoch. A quote far from the median, at less than half or more than twice it, is left out.
- **Who reports:** only arcades with draw weight, at most one quote per tier per audit period each. That's the same
  sybil limit storage already has: weight costs real disk.

### Together

ARCADE per HU = (hosting index in a coin) ÷ (swap index in that coin), averaged across the coins that have both.
It's published per epoch in the chain's state, so every arcade reads the same number.

## What it's used for, step by step

Each step is useful alone, and the risky ones come last.

**Phase 1: read only, no rules.**
- A dashboard: ARCADE's swap price per coin, from reports read off chain, plus the operator's own hosting cost.
- "Hosting cost covered": what this arcade earned last month (pot payouts, relay fees, compute) against what it pays
  for its machine.

**Phase 2: operators price in hosting units.**
- The relay price can be set as "x% of an HU-S per GB" instead of a fixed ARCADE number, converted with the
  dashboard's price. The operator still decides.
- **Paying the host from earnings:** when the arcade's earnings in a coin the host takes reach the next invoice, it
  swaps ARCADE for that coin on the market and pays the invoice address. This only happens with the operator's
  switch on and a monthly cap, the same way relay payments work (v0.1.96).

**Phase 3: the oracle on chain.**
- Swap reports and hosting reports become items in blocks (§2.4-style proofs).
- The two indexes and ARCADE per HU go into the chain's state each epoch.
- Nothing pays by them yet. Testnet watches them settle down.

**Phase 4: fees in hosting units (a rules version).**
- Blob storage, tag check-ins and the minimum relay fee are set in HU and charged in ARCADE at the epoch's price.
  Users pay a steady real price.
- The pot's income tracks real usage.

**Phase 5: rewards that pay for hosting (a rules version).**
- The block reward is set by need, not fixed: each epoch's pot aims at (the weight on the network, in HU) × (ARCADE
  per HU) × a margin over cost. It's capped at ±10% per epoch and at a maximum supply schedule, so it can't inflate
  without limit.
- More hosting gives more pay, and paying for it brings more hosting. If ARCADE's price drops, each HU costs more
  ARCADE, so rewards in ARCADE rise. If no one buys ARCADE, the supply schedule is the backstop. Martin decides the
  cap.

**Phase 6: compute on GPUs.**
- Compute jobs (LLM answers, renders) are priced in HU-G.
- An arcade with a GPU registers its tier, like storage replicas. Its weight for compute comes from passed **compute
  checks**: a checker sends a job with a known answer, or the same job to two arcades and compares, with audits like
  storage.
- Pay comes from the user's fee plus a compute share of the pot.

## Risks

- **Thin markets.** Until ARCADE trades in volume, the swap index is noise. Phases 1–3 only show it. Fees and rewards
  wait until there's enough volume for long enough (phase 4's gate: N epochs in a row above a volume floor).
- **Manipulation.** A rich actor could trade at a fake price for long enough to move the median. The clamps limit
  how fast it moves, and the volume needed grows with real volume. Rewards (phase 5) also have the supply cap.
- **Fake hosting quotes.** These are weight-limited and medianed, with outliers dropped. A provider's actual price
  page hash makes a lie checkable by anyone.
- **Inflation.** Phase 5 could print too much if the price index runs away. That's why there's a ±10% clamp per epoch
  and a hard supply schedule, and phase 5 needs Martin's go-ahead on mainnet.
- **Verifying compute.** It's harder than storage. Phase 6 starts with jobs that can be checked: known answers, and
  duplicated work on random jobs.
- **Complexity.** Each phase ships alone and can stop there. The network is useful at every step.

## Ashburn: the first data point

The relay VPS costs about $7 a month and takes some of the coins arcades already support, which makes it HU-S-sized.
- **Phase 1:** its operator's dashboard shows relay earnings against that cost.
- **Phase 2:** its relay price follows "cost per GB of its traffic", and its earnings can pay its own invoice.

On testnet, ARCADE has no market value, so this tests the plumbing, not the economics.

## Decisions for Martin

1. The three reference machines (HU-S, HU-L, HU-G) and their specs.
2. Which coins count for the indexes: those with both swaps and hosts that take them (LTC and DOGE first?).
3. Phase 5's margin over cost, and the hard supply schedule it can't pass.
4. Whether hosting payments (phase 2's auto-pay) may swap ARCADE automatically, or only pay from coins already held.
5. Mainnet: start with phase 1–3 only, and switch on 4 and 5 by rules version once the indexes have been steady.
