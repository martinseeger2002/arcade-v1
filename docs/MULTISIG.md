# Multisig on ARCADE's UTXO chains: a plan

Status: **design agreed; phases 1 (scripts, addresses, checking), 2 (xpubs, descriptors), 3 (PSBTs), 4 (watch-only vaults), 5 (spending) and 6 (co-signing over the mesh) built.** For review before any code. Where this page and the code disagree later,
the code is right and this page has a bug.

Goal: a coin can be locked so that **M of N keys** must sign to spend it, on every UTXO chain ARCADE keeps where the
chain's rules allow it, with one wallet format, one signing flow and one backup story across chains.

## 1. What the code understands today

| Area | Multisig today | Where |
|---|---|---|
| Consensus (other chains) | Full: P2SH, P2WSH, Taproot script paths are run by libbitcoinconsensus with each chain's flags; sigops counted as Core does (`CHECKMULTISIG` = 20 inexact, n exact in P2SH / witness) | `arcade-chaindb/src/rules.rs`, `arcade-chain/src/sigops.rs` |
| Consensus (ARCADE chain) | Bare multisig and P2SH (so P2SH-multisig) with strict rules: NULLDUMMY, strict DER + low S, ALL / ALL\|ACP / SINGLE\|ACP, no `OP_CODESEPARATOR`, 201 ops, 520-byte pushes | `arcade-arc/src/script.rs` |
| Ledger / indexes | `classify` knows `Multisig {m, keys}` (bare); P2SH and SegWit outputs get addresses (`address_of`, `wallet_address_of`, P2WSH as bech32 v0, 32-byte program) | `arcade-ledger/src/script.rs`, `address.rs` |
| Wallet keys | One 12-word BIP39 phrase; single keys only: m/44', 49', 84', 86' per chain, tag i = index i. **No xpub type, no public-only derivation** | `arcade-wallet/src/keys.rs` |
| Wallet signing | P2PKH, P2WPKH (native / in P2SH), Taproot key path (BIP86), plus fixed scripts: HTLC, prize pool, DigiDollar locks, trade legs. **No multisig signer** | `arcade-wallet/src/tx.rs`, `htlc.rs`, `legs.rs`, `taproot.rs` |
| Wallet verify | P2PKH, P2PK, P2WPKH, P2SH-P2WPKH, P2TR key path, prize-pool P2SH. **P2SH/P2WSH multisig and Taproot script paths come back "not checkable"** | `arcade-wallet/src/verify.rs` |
| PSBT | BIP174 v0 reader/writer exists for website signing: needs `non_witness_utxo`, signs this wallet's P2PKH inputs (and one inscription-reveal lock), finalizes P2PKH. Knows key 0x04 (redeem script) only for the reveal; no 0x05 witness script, no 0x06 BIP32 derivation, no combiner | `arcaded/src/psbt.rs`, `psbtbuild.rs` |
| Watch-only | Address watch exists (`iswatchonly` in the Core-style RPC) but per address, not per descriptor | `arcaded/src/rpc.rs` |
| Earn payouts | Payout is a 21-byte address and `pay_to` accepts the chain's P2SH version, so **an ARCADE-chain P2SH multisig payout address already works** with no protocol change | `arcade-arc/src/registry.rs` |

So: the chains already *accept* multisig (we validate it), the indexes already *see* it, but the wallet can't
*make, show, sign, check or track* it.

## 2. What each chain allows

From `arcade-chain/src/params.rs` (`script_flags`, `Format`).

| Chain (key) | P2SH | SegWit (P2WSH) | Taproot | CSV | Best multisig form | Notes |
|---|---|---|---|---|---|---|
| Bitcoin `btc` | yes | 481,824 | 709,632 | yes | `wsh(sortedmulti)`; later `tr` | MuSig2 possible (key path) |
| Litecoin `ltc` / `ltctest` | 218,579 | 1,201,536 | 2,257,920 | yes | `wsh(sortedmulti)` | **MWEB has no scripts**: no multisig inside MWEB; a vault can peg in/out only via normal outputs |
| DigiByte `dgb` / `dgbtest` | yes | 4,394,880 | 21,168,000 | yes | `wsh(sortedmulti)` | DigiDollar locks are their own scripts; not mixed in |
| Bellscoin `bel` | yes | 144,000 | 144,000 | yes | `wsh(sortedmulti)` | P2SH byte 30 gives "D…" addresses like Dogecoin P2PKH, so the wallet offers no nested `sh(wsh)` kind |
| Junkcoin `jkc` | yes | 1,145,000 | 1,155,000 | yes | `wsh(sortedmulti)` | |
| Dogecoin `doge` | yes | never | never | **no** | `sh(sortedmulti)` | CLTV yes, CSV never |
| Pepecoin `pep` / `peptest` | yes | never | never | no | `sh(sortedmulti)` | CLTV from 1,000 |
| Luckycoin `lky` | yes | no | no | no | `sh(sortedmulti)` | P2SH byte 5; signature hashes start with a chain-id prefix from block 276,500 (`luckycoin::sighash_prefix`), which the wallet's signers don't add yet: needed before LKY vaults |
| Flopcoin `flop` | yes | no | no | no | `sh(sortedmulti)` | |
| Shibacoin `shic` | yes | no | no | no | `sh(sortedmulti)` | |
| Wojakcoin `wjk` | yes | no (despite `wj` hrp) | no | no | `sh(sortedmulti)` | P2SH only, no CLTV |
| Kotia `kot` | yes | no | no | no | `sh(sortedmulti)` | tx carries a time that the sighash covers; scripts not checked here. **Last** |
| ARCADE chain `arc` | yes | no | no | height lock | `sh(sortedmulti)` | our own interpreter: ALL only for vaults |

**Address collision:** Dogecoin, Pepecoin, Flopcoin and Shibacoin all use P2SH version 22. The same vault on two of
them has the *same address string*. Every vault, PSBT and request must name its chain explicitly; an address alone is
never enough.

### Limits

| Form | Hard limit | Standard (relayed) | Practical cap for ARCADE |
|---|---|---|---|
| `sh(multi)` legacy | redeem script ≤ 520 bytes, ≤ 20 keys | ≤ 15 sigops in the redeem script; scriptSig ≤ 1,650 bytes | **15 compressed keys** (15 × 34 + 3 = 513 bytes) |
| `wsh(multi)` | ≤ 20 keys per `CHECKMULTISIG` | witness script ≤ 3,600 bytes, ≤ 100 stack items | 20 keys |
| `tr(multi_a)` | script ≤ 10,000 ops budget | 999 keys policy | not needed v1 |
| bare multisig | | ≤ 3 keys standard, often refused | **never** for vaults |

Only compressed keys. `sortedmulti` (BIP67 key order) everywhere, so co-signers can build the same address without
agreeing on an order.

### Size and fee per input (2-of-3)

| Form | Input size | vs single-sig |
|---|---|---|
| P2PKH | 148 bytes | 1× |
| `sh(sortedmulti(2,…3 keys))` | ~297 bytes (OP_0, 2 × 73 sig, 105-byte redeem) | ~2× |
| P2WPKH | 68 vB | 1× |
| `wsh(sortedmulti(2,…3))` | ~104.5 vB (41 base + 254 WU / 4) | ~1.5× |
| `tr` MuSig2 key path | 57.5 vB | less than single P2WPKH |

On the Dogecoin family fees are per started kB (`fee_for_size`), so a vault spend with a few inputs costs about the
same as a single-sig one; consolidation matters more on BTC/LTC.

## 3. Options and recommendations

### 3.1 Wallet format: output descriptors + PSBT, everywhere

- A **vault** is one descriptor (BIP380–383 with checksum) plus a name and a chain:
  - Dogecoin family and the ARCADE chain: `sh(sortedmulti(2,[fp/48'/3'/0'/0']xpubA/<0;1>/*,…))`
  - SegWit chains: `wsh(sortedmulti(2,[fp/48'/coin'/0'/2']xpubA/<0;1>/*,…))`
  - `/<0;1>/*` (BIP389) gives receive and change branches from one string.
- **PSBT (BIP174 v0) for every chain.** PSBT v0 doesn't need SegWit: legacy inputs carry `non_witness_utxo` (the
  whole previous tx), which the existing `psbt.rs` already demands. Add: 0x04 redeem script, 0x05 witness script,
  0x06 / global 0x01 BIP32 derivations and xpubs, a combiner, a multisig finalizer.
- One **ARCADE proprietary global field** (key `0xFC 06 "ARCADE" 00`): the chain's genesis hash (unique per chain
  and network, where a key name like `pep` is ours alone). A signer refuses a PSBT without it, or for a chain it
  doesn't expect (the address-collision point above). Built in phase 3 without the descriptor checksum: each
  co-signer checks every input against its own copy of the vault instead (key paths → address → the coin's
  script), which a checksum written by whoever made the PSBT couldn't prove.
- Derivation: one new account per vault on each co-signer's phrase, BIP48 `m/48'/coin'/vault'/type'` (`2'` = wsh,
  `1'` = sh-wsh, `0'` = legacy P2SH, ours since BIP48 names none). Because the descriptor carries every origin,
  recovery never depends on guessing paths.
- The vault number (`vault'`) is counted once per phrase across all chains, not per chain: the Dogecoin-family
  chains share a coin type and a P2SH byte, so vault 0 on Dogecoin and vault 0 on Pepecoin would otherwise be the
  same address on two chains.
- Descriptors write `xpub` on main chains and `tpub` on test chains, whatever the chain (Dogecoin's own `dgub`
  is not used): the chain is named next to the descriptor.
- Rejected: a home-made "PSBT-like" format for non-SegWit chains (PSBT v0 already covers them and keeps us compatible
  with Sparrow / Electrum / Core for BTC/LTC); bare multisig (non-standard); unsorted `multi` (order agreement errors).

### 3.2 Who the co-signers are (in build order)

1. **One person, several keys (the "vault")** — e.g. 2-of-3: the home arcade's key, the ARCADE app on a phone or
   laptop, and an offline backup phrase. Covers theft of one device and loss of one key. Smallest useful thing.
2. **Several people on one arcade** — accounts (`@tags`) on the same arcade each hold a key; the arcade passes PSBTs
   between them internally. Shared treasuries, families.
3. **People on different arcades, over the mesh** — the PSBT travels in sealed boxes (the same mechanism as
   `id.box_put`: only the two ends can read it; the relay passes it on). Every co-signer's own arcade rebuilds the
   amounts from its own chain data before signing; nothing is trusted from the sender.
4. **Hardware wallets / other wallets** — PSBT file and QR (BBQr/UR) export-import. BTC/LTC first, where Sparrow,
   Core and hardware wallets already speak `wsh(sortedmulti)`. For the Dogecoin family support outside ARCADE is thin,
   so ARCADE app + paper phrase is the realistic set.

All three co-signer models (1–3) are designed for from the start: the vault record names each co-signer as
`{xpub, origin, where}` with `where` = this arcade's own key, an account on this arcade, or another arcade's
mesh id, so a vault made in phase 4 for one person can gain co-signers on other accounts or arcades without a new
format. They are *built* in that order (phases 4–6).

Taproot (MuSig2 key path, or `tr(…, multi_a)` script path) is **later**: smaller and private, but MuSig2 needs a
two-round nonce exchange with strict no-reuse rules and its own audit. Only BTC/LTC/DGB/BEL/JKC benefit.

### 3.3 How ARCADE features fit

| Feature | v1 of multisig | Why |
|---|---|---|
| Coin control (`coin-control` session, in progress) | Vault coins are their own account; coin control picks within one vault; never mixed with single-sig coins in one spend | one PSBT = one set of signers |
| Inscriptions / ords held | A vault address can **hold** inscriptions; `ordsafe` keeps them out of plain sends, same as today | indexes track outputs, not key types |
| Inscribing *from* a vault | Later: the commit spends vault coins (co-signed); the reveal lock stays a single fresh key; the inscription lands on a vault address | reveal-lock signing is a special case today |
| DRC-20 / tokens | Hold: yes. Transfer: later (two co-signed txs) | |
| APROTO records, @tags, feed | Not in the first phases; **a vault @tag is a later phase** (8). The ledger already takes a P2SH first input as a sender, so the protocol side may need little; the work is the waiting-for-M flow for posts, likes and tips | every action waits for M signatures |
| Digi-ID, ARCADE ID sign-in, app-signs-each-action | **Identity stays single-key.** A site may *request a vault spend*; the app signs its share and the PSBT waits for the others | sign-in answers in seconds; multisig can't |
| Swaps, trade legs, HTLCs | Not from vaults in v1 | they need SINGLE\|ANYONECANPAY or time paths; review separately |
| Earn ARCADE payout | A vault address on the ARCADE chain can be the payout address **today** | `pay_to` accepts P2SH |
| Borrowed chain data (mesh) | Watch-only vault balance works from borrowed data like any address | |
| Litecoin MWEB | Not possible inside MWEB | no scripts there |

### 3.4 Backups, recovery, watch-only

- Each co-signer backs up **their own 12 words** (as today) **and the vault descriptor**. Without the descriptor,
  M phrases are *not* enough unless every other co-signer's xpub is known: we say this loudly at creation.
- The descriptor is shown as text and QR, with its checksum, and the app makes each co-signer confirm the first
  receive address on their own screen (the anti-swap check: an attacker who substitutes an xpub changes the address).
- A choice at setup: also keep an encrypted copy of the descriptor in the mesh store, readable by co-signers only.
- **Watch-only**: any arcade (or the desktop app with no keys) imports the descriptor and shows balance, coins,
  history and receive addresses. Note: a descriptor reveals every address of the vault (privacy, not theft).

### 3.5 What needs security review

- Signer checks (the core of the risk): amounts from `non_witness_utxo` hashed to the txid; **change must pay back
  to the same descriptor** (checked via the PSBT's derivation fields), anything else shown as a payment; fee shown
  and capped; chain named in the proprietary field; only SIGHASH_ALL for vault inputs.
- Address verification across devices; xpub substitution at vault creation.
- PSBT parsing as untrusted input (sizes, duplicates, key/value bounds); combiner merging conflicting fields.
- Same-key reuse across chains (one phrase → same pubkeys on Doge/PEP/Flop/Shic → same vault address): fine for
  funds, but the UI must never show a balance from the wrong chain.
- Later, MuSig2 nonce handling (never reuse, never let a peer choose ours).
- A review pass by someone outside the team before mainnet, as for the wallet extension.

## 4. Phased plan

Each phase is one PR, aimed at ≤ ~400 lines (code, not counting test vectors), testnet only until phase 7.

| # | PR | Size | Tests |
|---|---|---|---|
| 1 | `arcade-wallet::multisig`: build/parse `sortedmulti` redeem and witness scripts, `sh` / `wsh` addresses per chain, limits (15 / 20 keys, compressed only). `verify.rs` learns P2SH-multisig and P2WSH-multisig (sigs in key order, NULLDUMMY) | ~300 | BIP67 vectors; Core `createmultisig` addresses for doge/pep/ltc/btc; sign-then-verify round trips; the ARCADE interpreter accepts what we build |
| 2 | `XPub` + public derivation (BIP32 CKDpub) in `keys.rs`; descriptor parse/print for `sh(sortedmulti)`, `wsh(sortedmulti)` with key origins, `<0;1>/*`, checksum | ~400 | BIP380/383/389 vectors; Core `deriveaddresses` cross-check |
| 3 | PSBT for multisig: move `psbt.rs` core into `arcade-wallet`; fields 0x04/0x05/0x06, global xpubs, ARCADE proprietary chain field; sign (legacy sighash + BIP143), combine, finalize | ~400 | BIP174 test vectors; 2-of-3 signed by three PSBT passes in any order equals one combined result; mismatched chain refused |
| 4 | Vault on the arcade, watch-only: `multisig.create` (adds this arcade's key), `multisig.import` (descriptor), `multisig.address`, `multisig.coins`, balance via the address watch; desktop app and `/ui/` | ~400 | create 2-of-3 on `peptest` across node 6 and the mini; addresses agree on every device; receive testnet coins |
| 5 | Spend: `multisig.propose` (coin control picks coins, builds PSBT), `multisig.sign`, `multisig.finalize` → `tx.broadcast`; approval card shows outputs, change check, fee | ~400 | 2-of-3 spend on `peptest` and `ltctest` with keys on two arcades + paper; refuses: change to foreign script, wrong chain, missing prev tx |
| 6 | Co-signing over the mesh: send/receive PSBTs in sealed boxes; "waiting for N more" state; several users on one arcade | ~400 | three arcades, one offline for a day; replayed and tampered PSBTs refused |
| 7 | Chains out: BTC/DGB/BEL/JKC `wsh`; ARCADE-chain vaults (Earn payout to a vault); mainnet behind a switch after external review | ~250 | per-chain address + spend vectors; regtest where we have it |
| 8 | Later: PSBT file/QR for hardware wallets (BTC/LTC); inscribing and DRC-20 transfers from a vault; a vault-owned @tag and records; any M-of-N; Taproot (`multi_a`, then MuSig2) with its own review | separate | |

Start on the **Dogecoin family (legacy `sh(sortedmulti)`) tested on Pepecoin testnet**: it is most of our users, the
legacy sighash code already exists, and the same code covers Doge, PEP, LKY, Flop, Shic, WJK and the ARCADE chain.
P2WSH follows in phase 1's own verify work and phase 7's chain list.

Phase 4 as built: the commands are `multisig.*` (`multisig.key`, `.create`, `.import`, `.list`, `.address`,
`.coins`, `.remove`), since `vault.put/get/delete` already name the password accounts' sealed records. Vaults are
kept per account, encrypted like the phrase; test chains only until phase 7; the mesh backup copy is offered at setup
and refused until phase 6 builds it. In the window (desktop app and `/ui/`, one set of files): **Wallet → Vaults**
on a test chain: new vault (this arcade's key shown to copy, two other keys pasted), add from a descriptor, and per
vault its receive address with QR, coins, co-signers (here / elsewhere) and the descriptor as text and QR. Coins are
read for every address up to 20 past the last receive address given out, on both branches.

Phase 5 as built: `multisig.propose` (largest coins first, or `coins` picked; change to the vault's next change
address; `all` sweeps), `multisig.card` (outputs as payment or change back to the vault, checked by key paths; fee;
signatures per coin; whether this arcade still sees each coin unspent; a warning over 5% fee), `multisig.sign`
(refused when a coin is seen spent here), `multisig.merge` (a co-signer's copy; conflicts refused),
`multisig.finalize` (`send` broadcasts as `tx.broadcast` does) and `multisig.drop`. Proposals are kept on the vault's
record (ten at most). A foreign output is shown as a payment, never as change. Borrowed chains: vault addresses are
followed apart from the wallet's own coins, and a vault's view also asks a lender for older coins.

Phase 6 as built: the mesh's service channel (`svc.ask`, crate::svcmesh) carries the PSBT, as a service named
`multisig` built into arcaded, instead of the website-pairing boxes (`id.box_put` is one arcade's ten-minute relay,
not mesh-wide). Mesh links are end-to-end encrypted, through relays too. Each co-signer key of a vault names the
arcade it is on (its node id: this one for our own key, others with `multisig.set_node`); `multisig.share` sends a
proposal to every named arcade. An arcade takes a PSBT only from an arcade named on one of its vaults and only when
every coin is that vault's; it is merged into the proposal kept there and waits for its user (nothing is signed on
arrival); the proposal keeps who sent it. Several people on one arcade: each account keeps its own copy of the
vault and signs in turn (accounts switch in the window). "Waiting for N more" is the card's signatures of `needed`.

## 5. Decisions (2026-11-24)

- Co-signers: all three models (one person's devices, several people on one arcade, people on different arcades) are
  planned in the format from the start; built in that order.
- First chain family: Dogecoin family, legacy `sh(sortedmulti)`, tested on Pepecoin testnet.
- Shape: **2-of-3 only** at first; any M-of-N later.
- Vault @tags / records: **yes, in a later phase** (8), not in the first ones.
- Derivation (Q4): **BIP48** for every vault, `m/48'/coin'/vault'/type'`: `2'` wsh, `1'` sh-wsh, and `0'` for legacy
  P2SH (BIP48 names no legacy type; ours, written in each key origin of the descriptor).
- Descriptor backup (Q6): **a choice at vault setup**: keep it yourself only (text/QR), or also store an encrypted
  copy on the mesh that only the vault's co-signers can read.
