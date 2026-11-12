# The market and atomic swaps — PLAN

Status: **being built** (see "Built and tested" at the end). Written 2026-11-10 on the GX10 from a read-only audit of the
original arcade's code and of one live original arcade. Asked for by Martin: the token market of the
original arcade on V1.0, usable from an inscribed app, with cross-chain swaps that stay trustless and
work with the original arcades.

## Decisions this plan rests on

1. **No new ledger rule.** The original's lock scripts already carry tokens (a lock's address is an
   ordinary ledger address), and V1.0 replays the same ledger. A ledger-level lock would split V1.0's
   ledger from the original's, so it is not used.
2. **Same wire formats.** V1.0 uses the original's lock script, order form, trade messages and mesh rooms
   byte for byte, so a V1.0 arcade and an original arcade trade with each other and every order already
   listed stays valid.
3. **Each arcade answers its own orders with its own key.** V1.0's daemon holds the wallet and stays on,
   so it runs the trade itself. That removes the original's two trust modes for anyone running their own
   arcade (below).
4. **A page never signs.** An inscribed market passes typed terms; the arcade builds the transaction and
   shows a card made from the parsed transaction.

## What the original does (five ways to settle)

| Way | Atomic | Trades |
|---|---|---|
| A. On-chain order book (ledger types 25 order, 26/27/28 cancel, 29 take) | token for token: yes, both sides are ledger balances. Token for coin: one transaction and the ledger holds the maker's tokens in reserve, **but the coin payer carries a race** (see "Limit orders and partial fills") | token for coin and token for token on one chain, partial fills. Active on Pepecoin testnet only |
| B. Pre-signed legs (ledger types 5 swap, 6 offer, 7 ask, 8 bundle) | yes, one transaction; seller signs SINGLE\|ANYONECANPAY over the price output, a bid signs ALL\|ANYONECANPAY | NFT or token for coin or token on one chain; ord listings; player-to-player bundles |
| C. Lock-script swaps | yes, a hash-and-time lock on both chains | coin, token, NFT, ord, DRC-20/PRC-20 across chains; whole amounts |
| D. Held cross-chain book | **no, custodial** | testnet assets for mainnet coin |
| E. Fill-while-away helpers: delegated orders (the node holds the order's key, capped), standing buy lots (the node holds pre-signed lots) | **no, trusts the node** | — |

Also accepted by choice: a coin swap below the "big swap" line acts on an unconfirmed lock (a
double-spend risk up to that line). A leg that is not plain coin always waits for 1 confirmation.

### The lock (C), exactly

- **Script** (P2SH):
  `OP_IF OP_SHA256 <H> OP_EQUALVERIFY <claimer key> OP_CHECKSIG OP_ELSE <time> OP_CHECKLOCKTIMEVERIFY OP_DROP <refunder key> OP_CHECKSIG OP_ENDIF`
  - H = SHA-256 of a 32-byte secret. The lock time is a **time, not a height** (two chains' heights mean
    nothing to each other; relative locks never activated on these chains).
  - Refused on a chain without CLTV, with a P2SH version that collides with another chain's, or with timed
    transactions.
- **Spends:** claim `<sig> <secret> OP_1 <redeem>`; refund `<sig> OP_0 <redeem>` with nLockTime at or past
  the lock time and a non-final sequence. Both SIGHASH_ALL.
- **Steps:** the taker makes the secret and locks first for 8 h. The lister checks that lock on the chain
  and locks for 4 h. The taker claims the lister's lock (only while more than 2 h 20 min remain on it),
  which puts the secret on that chain. The lister reads it and claims the taker's lock. If either side
  stops, each refunds after its own time.
- **Why 8 h / 4 h:** a block may be stamped up to 2 h ahead on these chains, so shorter locks would let one
  early block open both refunds. Don't shorten without redoing that arithmetic.
- **Tokens in a lock:** a token send whose last output is the lock's address credits the lock. The claim or
  refund spends the lock's coin as input 0 (which makes the lock the sender) with a send payload and the
  receiver last. The other side counts a token lock only when its own ledger shows the amount at that
  address, 1 confirmation or more.
  - Hazard: a claim with a bad payload spends the coin and leaves the tokens stuck. Only arcade-built
    spends are allowed.
- **DRC-20 / PRC-20:** a four-step transfer-note lock. **Ords:** the inscribed coin whole at output 0, a
  reserve at output 1.
- **Finishing while away:** each side pre-signs its claim and its refund and gives them to its own node,
  which can only add the secret and choose when to send.
- **The order** carries `{id, give, get, trade_key, take_key, msg_key, node, expires}` and fee terms, where
  a side is `{network, kind: coin|token|nft|ord|token20, asset, amount}`. The secret hash and lock times
  come later, in the `propose` message.
- **How the book travels:** mesh room `arcade-book` (each arcade speaks only for its own orders; changes
  every 1.5 s, the whole book every 30 s), presence rooms `arcade-orders/<address>`, trade messages in
  `arcade-inbox/<key>` and `arcade-talk/<key>`; node orders can also be inscribed in batches
  (`application/vnd.arcade.orders+json`).

### Live on one original arcade (2026-11-10)

- Same-chain book on Pepecoin testnet: 14 resting orders (8 token for coin, 5 coin bids, 1 token for
  token); 107 takes and 10 pair trades so far.
- Cross-chain: 7 open of 58 listed; 15 trades recorded, 12 done and 3 aborted. **6 of the 7 open orders
  are delegated** (the answering node holds their key).

## What V1.0 has and lacks

- **Has:** the ledger indexes the book, takes, pair trades, NFT offers and asks, swaps and bundles, with
  digests equal to the original's. A script address counts as a token sender and receiver, as in the
  original. Coins for any address, broadcast, the page send queue with cards, mesh rooms, and borrowed
  blocks checked for proof of work.
- **Lacks:**
  - any route that reads the book or the trades (`/r/book/<id>` answers 404);
  - builders for orders, cancels, takes, listings and bids;
  - signatures other than SIGHASH_ALL, a lock time other than 0, and P2SH inputs;
  - the lock script, the trade state machine, the mesh book and message rooms, the watcher, history, charts;
  - working `swap` and `escrow` doors (both are stubs) and any market door;
  - a script check on P2SH spends in the shared mempool, so a claim seen there could carry a wrong secret.

## Verdict per pair

| Pair | Original | V1.0 today | To build |
|---|---|---|---|
| coin for coin, two chains | works, atomic | no | step 5, 6 (**L**) |
| token for coin, two chains | works, atomic | no | step 7 (**M**, after 5) |
| token for token, two chains | works (needs the ledger on both chains) | no | step 7 (**S**, after token for coin) |
| token for token, one chain | works (testnet) | indexed, not usable | steps 1, 2 (**M**) |
| token for coin, one chain | works (testnet) | indexed, not usable | steps 1, 2, then 4 (**M** + **M**) |
| DigiDollar for DGB, one chain | not traded | holds and sends only | step 4 (**M**) |
| DigiDollar for a coin or token, two chains | not traded | no | step 7 (**M**, after 5) |
| DigiDollar for a token, one chain | not traded | no | step 7, strict mode (**S**) |
| held cross-chain book | works, custodial | no | **not ported** |

## Trust in V1.0

- **Delegated orders and standing buy lots go away for a person's own arcade.** Their reason was that an
  account lived on somebody else's node and its tab closed. In V1.0 the daemon holds the wallet, so it
  takes the lister's part itself: it answers `propose`, locks, claims and refunds with its own key, and
  takes a crossing sell order with its own coins. Nobody else holds a key or a signature.
- **Still trust, and said so on the card:** a person using somebody else's arcade from a phone or a web
  page. They either sign when they are back, or use the original's delegated form with its cap, on an
  arcade whose operator has turned that on. Plain scripts on these chains can't remove that.
- **Unconfirmed small swaps:** kept as the operator's setting, default as the original. The shared mempool
  shows a double spend sooner, which narrows the risk and doesn't remove it.
- **Compatibility with delegated orders already listed:** V1.0 takes them like any order (the lock holds
  both sides; the trust is the lister's, in their node).

## Limit orders and partial fills, trustless

Asked for by Martin, 2026-11-10: limit orders and orders that fill in parts, with less trust. Checked
against the original's code the same day (read-only).

**Words.** A limit order names an amount and a worst price. It first takes what is already on the book at
that price or better, best price first, and what is left rests on the book until it fills, is cancelled
or expires. A partial fill takes part of a resting order; the rest keeps its price.

### What the original already has

- **Token for token, one chain:** the ledger's pair order (type 25 with two tokens) is already a limit
  order. It matches the book in price order, fills in parts, and rests what is left. Both sides are
  ledger balances, so nothing moves unless both move. **Nobody is trusted.**
- **Token for coin, one chain:** a sell order rests with its tokens in the ledger's reserve, and a take
  (type 29) may take any part. A coin bid (type 25 selling the coin) reserves nothing: the ledger can't
  hold coins, so the bid on the chain is a notice.
- **Across chains:** a lock swap moves the order's whole amount. The lister's arcade refuses a `propose`
  whose amount differs, and the order leaves every book while one swap runs.

### Where trust is left today

1. **A take's payment.** A take pays the maker in plain outputs, which the chain moves whatever the ledger
   decides. The original covers two cases (a block's cancels apply after its takes; a take that finds the
   order short is filled from the maker's free balance). The case left: **the order was cancelled or
   emptied in an earlier block than the one the take confirms in. Then the maker keeps the coins and no
   token moves.** A maker who can get a cancel mined ahead of a waiting take can do this on purpose. The
   two-signature swap (type 5) has the same hole from the same cause: coins move by the chain's rules,
   tokens by the ledger's.
2. **Standing buy lots.** The node holds the buyer's signatures and could pay somebody else with a lot.
3. **Delegated orders.** The node holds the order's key, up to a cap.

### V1.0's design

No new ledger rule, and the wire stays the original's (decisions 1 and 2).

- **Token for token, one chain.** The pair order as it is. `place` builds it; the ledger does the limit
  matching and the partial fills.
- **Token for coin, one chain, two modes.** The card names the mode and what it risks.
  - **Fast (the original's way).** A limit buy takes resting sell orders at or under the limit, cheapest
    first, each with a take for the part it wants; what is left rests as a coin bid, and the bidder's own
    daemon takes later sell orders that cross it, with its own key. A limit sell rests as a sell order; to
    hit a resting bid it is placed at that bid's price for that bid's amount, and the bidder's daemon
    takes it. No key or signature leaves either arcade, which removes items 2 and 3. The race in item 1
    stays, so V1.0 bounds it: a take is never sent while a cancel of that order is in the shared mempool;
    one take pays at most a set amount (the person's setting, with the operator's ceiling), and the next
    slice goes only after the last one confirmed with its tokens moved. The most that can be lost to a
    cheating maker is one slice.
  - **Strict.** The same lock scripts as a cross-chain swap, with both locks on one chain: the tokens sit
    at a script address that only the secret or the refund time can move, and so do the coins. Nobody can
    lose anything. It takes four transactions and the confirmations between them, and it trades only with
    V1.0 arcades (an original arcade drops a one-chain lock order for a token). Offered where both sides
    run V1.0, and required above a size the person sets.
- **Across chains: a fill is its own lock pair.**
  - The order gains two optional fields, `partial: true` and `min` (the smallest fill, in the give side's
    units). An original arcade drops fields it does not know and shows the order as a whole one.
  - A V1.0 taker proposes any amount from `min` up to what is left, at the order's price or better for the
    lister (checked by cross-multiplying whole numbers, rounding against the taker). The lister's daemon
    accepts, and each fill has its own secret, its own two locks and its own refunds. A fill that stops
    refunds alone, and its amount returns to the order.
  - The order stays on the book while fills run. Its amounts are what is left and not promised to a
    running fill; the lister's arcade says the smaller amounts in the book room under the same id (the
    original files a changed body for an id from its home arcade). An original taker proposes exactly the
    amounts shown and gets them. A partial order is not inscribed in a batch, since an inscription can't
    shrink.
  - A V1.0 taker takes an original arcade's order whole only, because that arcade answers nothing else.
  - A limit order across chains takes crossing orders best price first, up to a set number of fills at
    once, and rests what is not in a running fill.
  - `min` exists because every fill pays its own lock fees and reserve. `terms` gives the smallest fill
    worth making on a pair.
  - Trust across chains is as before: none with 1 confirmation (the unconfirmed line set to zero). The
    side that made the secret can let a swap run out, which costs the other side hours and no coins; the
    lister's daemon limits how many fills one taker runs at once.
- **Items that stay whole:** an NFT, an ord, and one DRC-20 / PRC-20 transfer note.

### Trust after this

| Trade | Way | Who can lose |
|---|---|---|
| token for token, one chain | ledger pair order | nobody |
| token for coin, one chain, fast | ledger take, own daemon | the coin payer, at most one slice, to a maker who gets a cancel mined first |
| token for coin, one chain, strict | lock scripts on one chain (V1.0 with V1.0) | nobody |
| any pair, two chains | lock scripts, one pair per fill | nobody, at 1 confirmation |
| DigiDollar for DGB, one chain | one transaction, both sign all outputs | nobody |
| DigiDollar, two chains | lock scripts, the dollar lock a Taproot one | nobody (always 1 confirmation) |
| a person on somebody else's arcade | signs when back, or the capped key | as before: nobody, or the cap |

## The operator's trade fee

Asked by Martin, 2026-11-10: an operator may require a fee on trades made by people who use the arcade
without running their own, as a percentage the operator sets; and the many small fee coins this makes
should be combined, with no trust.

### What the original does

- One operator setting, "Your cut of trades made here", a percent kept as whole basis points (default 0,
  ceiling 100%). An offer the arcade builds carries the rate and the payee, `cut: {bps, to}`, never an
  amount; both sides work out `price * bps // 10_000` themselves.
- **One-transaction trades:** the buyer's transaction gets one more output paying the cut to the maker's
  arcade, on top of the price. The maker's side refuses to countersign when that output is missing or
  short. A cut under the dust line (0.01 on the Dogecoin family) is dropped and the trade goes free.
- **Lock swaps:** the fee sits in the locker's lock and the claim pays it as an output of its own to the
  locker's arcade (leg fields `fee_bps`, `fee_to`, `fee_share_to`); a refund returns it. It is a percent
  with a floor at the dust line. On a borrowed chain 20% goes to the lending arcade.
- So every trade leaves the operator one small coin. A 0.01 coin costs about 0.0015 to spend (148 bytes
  at 0.01 per kB), so it is worth spending, but hundreds of them make every later payment large.

### V1.0's design

- **The setting:** a percent on the operator's Market settings, default 0 (off), with a switch for whom
  it covers: hosted people only (default) or every order the arcade makes. Same wire fields and the same
  arithmetic as the original, so both kinds of arcade land on the same satoshi. The rate and the amount
  are on the card the person approves and in `terms.operator_bps`.
- **Merging fee coins (answers the question as asked).** The fee coins belong to the operator's own key,
  so one transaction with many of them as inputs and one output needs only the operator's signature.
  Nobody else is involved and there is nobody to trust. The daemon does it by itself: confirmed fee
  coins only, up to 200 inputs, when there are at least 20 and the sum is worth several times the fee;
  never coins that carry an inscription or a token. 200 coins of 0.01 cost about 0.30 to merge.
- **The rolling fee coin (makes no dust to begin with).** An offer from a V1.0 arcade may add one of the
  operator's own confirmed coins: `cut: {bps, to, coin: {txid, vout, value}}`.
  - The buyer's arcade adds that coin as an input and pays one output of `value + cut` to `to`.
    The operator pays for its own input: the output is smaller by that input's share of the network
    fee, so the trade costs the buyer what it costs on the original.
  - The buyer signs its inputs over all outputs. The maker's arcade checks the output and signs the
    maker's input and the fee coin, both over all outputs, last. Nobody can take the fee coin without
    paying it back with the fee, and nobody can change the fee after the buyer signed.
  - The fee can now be any size, under the dust line too, so no trade goes free and no small coin is
    made. The operator holds a few fee coins that only grow.
  - The fee coin is never input 0 (the ledger reads a class C sender from the first input) and never
    joins a class B transaction (its sender is the address with the largest summed inputs).
  - A coin is promised to one open offer at a time and freed when the offer ends. The arcade keeps a
    small pool (default 8) so trades don't wait on each other, and splits a grown coin to refill it.
    With no free coin the offer goes out the original's way.
  - An original arcade ignores `coin` and pays the plain output, which the V1.0 maker's side accepts.
    The rolling coin is used V1.0 with V1.0.
- **Lock swaps keep the original's rule,** fee in the lock and paid by the claim, so original arcades
  compute the same lock amounts. A claim is pre-signed and may be sent by any watcher, so it can't wait
  for an operator's signature. Those fee coins are merged as above. With partial fills, the smallest
  fill (`min`) defaults to an amount whose percent reaches the floor.

### Trust

The fee adds none. A buyer sees the rate before building, and a signature over all outputs fixes the
amount. The operator's coin moves only in a transaction the operator signed. A hosted person who signs
with their own key could trade elsewhere without the fee; the fee is the price of using this arcade's
order desk, not a lock on the person's coins.

## DigiDollar

Asked by Martin, 2026-11-10: the swaps must include DigiDollar. The original arcade holds and sends
DigiDollar and has no trade code for it, so this part is new work and trades V1.0 with V1.0.

### What the chain's rules allow

Read from DigiByte Core 9.26.7 (`src/digidollar/validation.cpp`, `src/policy/policy.cpp`) the same day.
**Read, not yet run:** the first build check below proves it on a chain before anything rests on it.

- Dollars sit at a zero-value Taproot output. The amounts, in cents, are in the transaction's
  `OP_RETURN "DD" 2 <cents> ...` record, one number for each zero-value Taproot output in order.
- A send (version `0x02000770`) is valid when dollars in equal dollars out and every dollar output
  holds $1 to $100,000.
- The rules take any zero-value Taproot output as a dollar output. They don't look at what its key
  commits to, and the spend is checked by the ordinary Taproot rules. So dollars can sit at an output
  whose only ways out are script leaves, which is what a lock needs.
- Outputs that carry DGB are passed over by the dollar rules, so one send can move dollars and pay DGB.
- A dollar coin can't be spent before it has 1 confirmation.
- A transaction without the DigiDollar mark may spend a dollar coin, and then the dollars are gone. Only
  arcade-built spends are allowed, as with a token in a lock.
- Below the Thaw Day height (mainnet 24,490,000, test chain 432,100) the chain stops all dollar movement
  during extreme price swings, and a lock's clock would keep running. From that height the rule is off.
  **V1.0 offers a DigiDollar lock on a chain only at or past that height.**

### V1.0's design

A side is `{network: dgb | dgbtest, kind: "dd", asset: "DD", amount}` with the amount in cents.

- **DigiDollar for DGB (one chain): one transaction.** Both assets move by the chain's own rules, so
  this pair needs no lock and no ledger.
  - The order rests on the shared book and the maker's own daemon answers it.
  - Inputs: the maker's dollar coins and the taker's DGB (or the reverse for a bid). Outputs: the dollars
    to the buyer, dollar change to the seller, the DGB price to the seller, DGB change, the record.
  - Each side signs its inputs over all outputs, the maker's daemon last, after checking its outputs
    and the record. The taker pays the network fee (DigiDollar's own rate, 0.35 DGB a kB, 0.1 at least).
  - **Partial fills:** any amount, as long as the fill and what is left are each $1 or more.
  - The operator's fee is a DGB output or the rolling fee coin, as for any one-transaction trade.
  - A person who is away and has no daemon can leave a signed leg for one whole dollar coin
    (SINGLE\|ANYONECANPAY over their price output; the signature covers the version, so it fits only a
    DigiDollar send). That leg fills whole.
- **DigiDollar across chains (for DOGE, PEP, LTC, a token, ...): a lock swap** with the same steps, hash,
  times and keys as every other lock. Only the DigiDollar lock's form differs:
  - A Taproot output with no key path (the internal key is the standard point nobody holds a key for)
    and two leaves:
    `OP_SHA256 <H> OP_EQUALVERIFY <claimer key> OP_CHECKSIG` and
    `<time> OP_CHECKLOCKTIMEVERIFY OP_DROP <refunder key> OP_CHECKSIG`.
  - The lock transaction is a DigiDollar send. The dollars go to that output, and a small DGB reserve
    goes to the same address, since a dollar coin carries no DGB to pay for its own spend.
  - The claim or refund is a DigiDollar send that spends both by a leaf and pays one dollar output.
    Both are pre-signed like any other lock's.
  - The other side counts the lock when the address is the one it computes itself from H, the time and
    the two keys, the creating transaction's record gives the amount at that output, the reserve is
    there, and it has 1 confirmation. The unconfirmed fast path never applies to a DigiDollar leg.
  - **Partial fills:** one lock pair per fill; a fill and what is left are each $1 or more, and one
    fill is at most $100,000.
  - The operator's fee on a DigiDollar leg is a second dollar output of the claim when it reaches $1,
    and dropped under that (the original drops a cut under the dust line the same way).
- **DigiDollar for a ledger token on DigiByte:** strict mode only, a dollar lock and a token lock on the
  one chain. The ledger's take counts coin outputs, so fast mode can't pay in dollars.
- **Not in the market:** making and redeeming dollars. Those stay wallet actions.

### Trust

DigiDollar for DGB is one transaction signed over all outputs: nobody can lose. A lock swap with a
DigiDollar leg is as any lock swap at 1 confirmation: nobody can lose. The whole-coin signed leg trusts
nobody either; it only can't fill in parts.

## What V1.0's design adds

- **Watching:** the address index sees a lock's address with no import and no rescan. The swap runner
  reads its facts from the chain store in the block's own commit, so its state follows a reorg.
- **Speed:** shared mempools carry a claim, and the secret in it, to the other arcade in under a second.
  A secret is believed only when its SHA-256 equals H, and a mempool spend of a lock is a hint until it
  confirms.
- **More watchers:** a claim or refund is pre-signed and the secret is public once used, so any arcade may
  send it. An arcade can hand its pre-signed spends to arcades it links with as well as keeping them.
- **Borrowed chains:**
  - add the lock's script to the watched set, and take depth from V1.0's own checked headers;
  - take the claims-last part only on a chain that is installed or has 2 or more lenders;
  - a token or NFT leg needs the ledger index on that chain (an installed chain).

## The page API (for an inscribed market)

Shim `/r/market.js` gives `arcade.market`; door `market`. Replies are `{ok, ...}` or `{ok: false, error}`.
Reads are also plain `GET /r/market/*`. A side is `{network, kind, asset, amount}` in base units.

| Reads | Args | Reply |
|---|---|---|
| `markets` | `{network?, kind?, q?}` | `[{base, quote, venue: ledger\|legs\|lock, last, change_24h, volume_24h, asks, bids}]` |
| `assets` | `{q, network?}` | `[{network, kind, asset, name, divisible, icon}]` |
| `book` | `{base, quote, depth?}` | `{asks: [{order, maker, give, get, price, left, partial, min, venue, live, taking}], bids, spread}` |
| `order` | `{order}` | `{order, status: open\|taking\|filled\|cancelled\|expired, left, fills}` |
| `trades` | `{base, quote, limit?, before?}` | `[{id, at, price, base_amount, quote_amount, side, txids}]` |
| `candles` | `{base, quote, span, count}` | `[{t, o, h, l, c, v} or null]` (gaps kept) |
| `listings` | `{collection? \| asset?, network?}` | `[{listing, item, price, seller, expires}]` |
| `terms` | `{give, get}` | `{network_fees, operator_bps, lock_reserve, confirmations, lock_hours, cap, min_fill, modes: [fast\|strict], slice, supported, why_not}` |
| `mine` (needs wallet access) | `{}` | `{orders, swaps: [{id, status, step, locks, refund_at}], listings, offers}` |

| Writes (one card each) | Args | Reply | Settles by |
|---|---|---|---|
| `place` | `{give, get, expires?, partial?, min?, mode?, rest?}` | `{order, txid?, fills}` | a limit order: `give` and `get` set the worst price; it takes what crosses, then rests what is left (`rest: false` drops it instead). Settles by a ledger order, a signed leg, or an order on the shared book |
| `cancel` | `{order}` | `{txid?}` | a ledger cancel, spending the leg's input, or leaving the book |
| `take` | `{order, amount?, mode?}` | `{trade}` | a ledger take, a leg fill, or the start of a lock swap; `amount` under the order's whole is a partial fill |
| `offer` | `{item, price, expires?}` | `{offer}` | a pre-signed bid |
| `answer` | `{offer, accept}` | `{txid?}` | completing the bid |
| `withdraw` | `{offer}` | `{txid}` | spending the bid's coins |
| `refund` | `{swap}` | `{txid}` | a lock's refund (normally automatic) |

Events: `{arcade: "market", event: {type: order|trade|swap|book, id, status, step, txid?, why?}}`; swap steps
`proposed, accepted, locked, their_lock_seen, claimed, refunded, done, failed`.

**Never given to a page:** raw bytes to sign, a signature type, a signature, a secret, or the choice of
receiving address. One Yes covers that trade's later lock, never another amount or counterparty. The
card shows what is given, what is received, both fees, the lock hours and the confirmations. A per-app
Market switch on the app card, one card at a time, and a limit on orders per minute. Pre-signed spends
stay in the daemon. The original's `/r/book/<id>` keeps its exact shape so pages written for it still run.

## Build order

1. **Market reads (S).** `ledger.book`, `ledger.trades`, `ledger.pairs` over the indexed tables;
   `/r/book/<id>` in the original's shape plus bids; `/r/market/{markets, book, trades, candles}`.
   Check: equal to the original's book for every token on testnet.
2. **Ledger order, cancel and take (M).** Payload builders for types 25 to 29 through the approval queue;
   `taking` from the mempool. Check: an order placed on V1.0 is taken from an original arcade and the
   reverse; digests stay equal.
   - Limit orders in fast mode: the matching walk for `place`, takes in slices, no take while a cancel is
     in the shared mempool, and the daemon taking sell orders that cross its own resting bids.
3. **The `market` door, and real `swap` and `escrow` doors (M).** The API above in the app window and the
   daemon; the Market switch.
4. **Two more signature types and one-transaction legs (M).** ALL\|ANYONECANPAY and SINGLE\|ANYONECANPAY
   only as the original's safety rules allow (never NONE; SINGLE only over the seller's price; a relist
   only cheaper); listings, bids, bundles. Port the original's safety tests.
   - The operator's trade fee: the setting, `cut` in offers, the check before countersigning, the
     rolling fee coin and its pool, and the daemon's merge of fee coins.
   - DigiDollar for DGB in one transaction: the order on the shared book, the two-sided build, the
     record check before the maker's daemon signs, partial fills with the $1 rule.
5. **Lock-script swaps (L).** The script, P2SH signing, lock time and sequence; the original's `next_step`
   as a pure function with its vectors; the runner in the daemon; the watcher; the P2SH script check in
   the shared mempool; borrowed-chain rules; a fee plan for pre-signed spends (several fee levels, or an
   output a child can pay for).
   - One lock pair per fill: the lister's daemon accepts any amount from `min` to what is left, at the
     order's price or better, and keeps what is left across running fills, refunds and restarts.
6. **Mesh market rooms (M).** `arcade-book`, presence, inbox and talk rooms in the original's form;
   inscribed order batches. Check: an original arcade lists a V1.0 order and a V1.0 arcade lists the
   original's 7.
   - Partial orders: `partial` and `min` in the order, smaller amounts said under the same id. Check: an
     original arcade shows a half-filled V1.0 order as a whole order of what is left, and takes it.
7. **Asset locks (M, then L).** Token and NFT locks and their spends, the ledger amount at a lock address,
   ord locks; then DRC-20 / PRC-20.
   - Strict mode on one chain: a token lock and a coin lock on the same chain, V1.0 with V1.0.
   - DigiDollar locks. **First, the proof on a test chain:** $1 locked at a script-only Taproot
     output, claimed by its hash leaf, and a second one refunded by its time leaf. Nothing else of
     this item is built before that passes. Then the Taproot lock and its leaf signatures, the DGB
     reserve, reading a lock's amount from the record, the Thaw Day gate, and the leaf check in the
     shared mempool. Check too that an original arcade neither lists nor answers a `dd` order; if it
     would, these orders travel in a room of their own.
8. **History and charts (S).**
9. **Escrow (M).** For the `escrow` door; the market doesn't need it.
10. **Public reads (S to M),** if V1.0 serves pages to web visitors: `/r/market/*` reads public, writes
    through browser-held keys.

Not planned: the held cross-chain book.

## Tests that decide "done"

Every pair in the verdict table, V1.0 with V1.0 and V1.0 with an original arcade in both roles, on
testnets, each with: a clean run; the other side stopping after each step (both refunds); a reorg under
a lock and under a claim; a lender leaving mid-swap; a claim with a wrong secret in the mempool; a
restart of the daemon at every step.

Limit orders and partial fills: an order filled by three takers in parts, two of them at once; one fill
refunded while the others finish, and its amount back on the book; a limit order that takes two resting
orders and rests the remainder; a take against an order cancelled one block earlier, losing at most one
slice in fast mode and nothing in strict mode; an original arcade taking what is left of a partly filled
order.

The operator's fee: a hosted maker's trade at 1% taken by a V1.0 arcade (one fee coin in, the same coin
out larger, no new coin) and by an original arcade (a plain fee output); a buyer's transaction with the
fee output short, refused; a fee under the dust line collected through the fee coin; two trades at
once using two pool coins; 40 plain fee coins merged into one by the daemon; a lock swap whose claim
pays the fee, equal to the original's amounts.

DigiDollar: $50 sold for DGB in one transaction; the same order filled by two takers in parts; a fill
that would leave under $1, refused; a record that pays the buyer short, refused by the maker's daemon;
a dollar lock claimed and one refunded on the test chain; DigiDollar for a coin on another chain, a
clean run and both refunds; a spend of a dollar lock that lacks the DigiDollar mark, never built; a
lock offered below the Thaw Day height, refused.

## Answered by Martin, 2026-11-10

1. **Unconfirmed small coin swaps: the operator's setting.** The default is the original's size line; an
   operator can set the line to zero, and then every swap waits for 1 confirmation.
2. **Delegated orders listed by original arcades: V1.0 takes them** like any order.
3. **Holding a capped key for people who use an arcade without running their own: the operator's choice.**
   The form and the cap are the original's. This plan's default is off; the default was not asked.
4. **Order of work: the deployed apps first** (`docs/APPCOMPAT.md`), then this plan, same-chain (steps 1 to
   4) before cross-chain (5 to 7).
5. **Limit orders and partial fills, with less trust** (asked the same day): the section above. The
   default mode for token for coin on one chain was not asked; this plan's default is fast up to the
   slice size and strict above it.
6. **The operator's trade fee** (asked the same day): the section above. Not asked: whether the fee also
   covers the operator's own orders (this plan: no, hosted people only by default).
7. **The new-order card** (said about the mockup the same day): one simple card like the original's pair
   card. Buy or sell, an amount, an asset, a price, an asset. Both asset boxes complete as you type,
   over coins and tokens. The side a person pays from lists only what their wallet holds, and an order
   for more than they hold can't be placed; the card says the most they can place.

8. **DigiDollar in the swaps** (asked the same day): the DigiDollar section above. It trades for DGB in
   one transaction and for everything else by a lock, V1.0 with V1.0. The chain's rules were read, not
   run; the proof on a test chain comes first in the build.

## Coin icons

Inscribed on Pepecoin testnet, 2026-11-10 (JKC and SHIC 2026-11-11, each from its project's own logo in its
core repo; ARCADE and ARCADE testnet 2026-11-11, the desktop's green and purple cabinets), PNG 96 x 96, by a test account (none of Martin's addresses).
`assets` gives these as `icon` for a chain's coin. Each was read back from an original arcade and from
V1.0 with the source file's SHA-256.

| Coin | Inscription |
|---|---|
| ARCADE | `698fa4a636e90278ef6ca3f1b26806e634b9ee323d0350003f2a472a2fa50455` |
| ARCADE testnet | `e2982d87007506fab2380fbc5cd2003c6ac09b304a670772063affb2a8abf49f` |
| BEL | `aeb761003d131b0ec643713b9663e00034345bbb4bf9d40744867892b5425ec2` |
| DD | `44a1c86f8cacccc7227eb4f2fd3ae85ece5a07e99a21cc1157e26bca3d8fa20a` |
| DD testnet | `e5d1b9dd393681c87170417f3dd9e8245fef7877eb7796a608288f9168433594` |
| DGB | `5796bfe950c32aed2a3ad848eb0afe23ded4f62dabc7b85b50154c0762999d9a` |
| DGB testnet | `807c7cdb64000a3e67f5f34bdc64e883d034ad7c26c106cbc5b5323dde79b682` |
| DOGE | `8c6f3bb878b2ab9cf80837020576b5eeb770279be60fd471299f2b84482fb91a` |
| FLOP | `9ad22450724ed29cce47782b8911a288cebd62af68a1bac894a142c5ffb928fd` |
| JKC | `ef865a7894b24cca72b2ef39e915064bbccdca1a9fc403f569a3292460eab753` |
| KOT | `0ee77138d2e23272db4d84c9d440cb55e2e8d9c5bdb1b80bf6c44045dd2fb33a` |
| LKY | `5502e6b46f64756b88441d536d014bcd991abc63705671569162dde6514aa29f` |
| LTC | `d8bff4aa74cabfabe238b89ba7cd8fc5ca11ce6b5d351ab4133996c941d22a3a` |
| LTC testnet | `8ba15b7c8c09dfdc06155db2acf587dc7d4d1b37dc00341c39ee4dc5c7a17f0f` |
| PEP | `ea060e94568de806493cdc76f5f590dd9d06e4521a873aa7c025540641e571ab` |
| PEP testnet | `5c94ebc6f08ea85d0480ee5ecf91617218b143d9cf27fbc13e7e5b7b247cb7f4` |
| SHIC | `82a7b197839d65058df00c98d29c09ca51c114bcece22b021051352294eb12f5` |
| WJK | `e1d07b7f8d6e25f784c0d6551c8145be0987e4f709225479db5ee8dc04a896b9` |

## Built and tested (2026-11-10 / 11)

On test chains only, between two test arcades in a private network, from test accounts.

- **Reads (step 1):** the ledger's books, trades and markets equal the original's on Pepecoin testnet
  (7 coin books, 2 pair books, 10 pair trades, 107 takes at 1,565,408).
- **Ledger orders, cancels and takes (step 2)** through the approval card; payload bytes equal the
  original's encoders; a take, a coin bid and its cancel settled, and the original's ledger agrees.
- **DigiDollar for DGB in one transaction**, whole dollars only, both directions, on DigiByte testnet.
- **DigiDollar locks:** the script-only Taproot lock (tree checked against BIP341's vectors), $1 claimed by
  its hash leaf and $1 refunded by its time leaf on DigiByte testnet.
- **The P2SH lock** byte for byte with the original's htlc.py (vectors made with it).
- **Swaps of any leg for any leg** (`swaps.rs`): coin, dd, token20 (the three-step note lock), token (ledger)
  and ord legs. Live: PEP for DGB, DD for PEP, SWPT (a test PRC-20) for DGB, an inscription for PEP; refunds
  of each side after its time, a token lock and a ledger-token lock each returned to their owner.
- **One taker at a time:** an order is held for the taker who asked for its terms; a second is refused.
- **The ARCADE chain's coin** (`arc`, `arctest`) as a leg, for any other leg. ARCADE's scripts take the P2SH
  hash lock and its signatures are V1.0's legacy ones, but its time locks are block heights only (no clock
  time). So an ARCADE lock ends at a height worked out from the swap's clock time and the chain's pace over
  its last 60 blocks: the taker's lock is 4 times the order's time and the maker accepts it only if it lasts
  3 times even with blocks twice as fast; the maker's lock is 1 time against the taker's 4, so it ends first
  unless blocks run 3 times slower than lately. Refunds there wait for the height, not the clock. Its waiting
  transactions are in its own pool (arcsvc). A unit test spends the lock by ARCADE's own script rules (claim,
  refund at its height, and a refund for an earlier height refused). Live, on a local ARCADE test chain and
  DigiByte testnet: 100 ARCADEtest for 50 DGBtest (maker's lock on ARCADE) and the reverse (taker's lock on
  ARCADE), both claimed by both sides; a taker's ARCADE lock refunded at its height when the maker never
  locked.
- **Relay:** a swap transaction is sent again, unconfirmed parents first, until a block holds it; outputs
  under the Dogecoin family's 0.01 soft-dust line pay its extra 0.01 each, or peers drop them.

Not yet: partial fills of lock swaps, mesh-wide order rooms (step 6), the operator fee, fee coins,
history for swaps and DigiDollar trades, original-arcade interop for lock swaps.

Asked by Martin, 2026-11-11: an operator who runs a website portal to the arcade marketplace needs a
way to enforce a maker fee on orders placed through it (beside the taker-side trade fee above).

Security fixes (re-audit at ba0e09a, 2026-11-11): a swap maker's lock takes only a 32-byte secret
(`OP_SIZE 32 OP_EQUALVERIFY`; its quote says `"sized": true`, and an older taker never finds the lock,
so both sides refund); an ARCADE coin leg can't be swapped for DigiDollars until the ARCADE chain has
OP_SIZE; a take goes ahead only on the terms its card showed; swap funding and borrowed-chain sends keep
inscribed coins out.
