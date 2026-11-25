# Litecoin MWEB in ARCADE

MWEB (MimbleWimble Extension Blocks, Litecoin LIP-0002/0003/0004) has been active on Litecoin
mainnet since block 2,265,984 and on testnet4 since 2,215,584. Every MWEB-era Litecoin block
ends with a HogEx ("integration") transaction and carries an extension block after its
transactions. Junkcoin (AUXPOW_MWEB format) uses the same layout.

## What the node does (all arcades, whatever the switch says)

| Part | Where | State |
|---|---|---|
| Ask peers for blocks with the extension block (`MSG_MWEB_FLAG`) | `arcade-chain/src/format.rs` | before this branch |
| Read the HogEx flag (tx flag 0x08, empty MWEB part) | `arcade-chain/src/tx.rs` | before this branch |
| Keep the extension bytes with the block, byte for byte | `Block::extra` | before this branch |
| Peg-out maturity (6 blocks) | `arcade-chaindb/src/db.rs`, `rules.rs` | before this branch |
| **Read the extension block** (header, inputs, outputs, kernels; ids by BLAKE3) | `arcade-chain/src/mweb.rs` | **this branch** |
| **Tie it to the block**: HogEx last and only there, HogAddr (witness v8) = BLAKE3 of the MWEB header, HogEx inputs = the block's peg-in outputs (witness v9) in order, kernel peg-ins and peg-outs match those outputs and the HogEx outputs | `mweb::check_block`, run by `check_block_body` on every MWEB chain | **this branch**; a failure is a damaged copy (another peer's copy may be fine), as Core marks most of these "mutated" |
| **HogEx amounts**: MWEB header height, input 0 spends the HogEx before, HogEx fee = MWEB fees, HogAddr value = pool before + supply change | `mweb::check_hogex_amounts`, in `rules::check_info` | **this branch**; runs with Litecoin's other value rules (the replay; not at connect until Litecoin's rules are `enforced`) |

Not checked (needs secp256k1-zkp and the MWEB UTXO set): range proofs, owner and kernel
signatures, the kernel sum (excess balance), the stealth offset, MMR roots and the MWEB coin set.
Before this branch a peer could hand us cut or made-up extension bytes and we would store and
serve them to other peers (Litecoin Core bans a peer that serves a mutated block); now such a
copy fails its checks and is fetched again elsewhere.

Tested on real blocks with `examples/mwebscan.rs`: mainnet activation (2,265,980-2,266,280),
the 601 blocks to the tip (3,218,956) and 300 random blocks in between: 0 failures, every HogEx
amount checked on the contiguous ranges (pool at the tip 512,572.75 LTC). Testnet4: see the
branch report. Unit tests use mainnet block 2,844,114 (one kernel with a peg-in and a peg-out)
and damage it in each way the checks must catch.

## The operator switch (`mweb.status`, `mweb.set`)

Off by default. Turning it on needs `accept: <notice version>`: the Options tab shows the notice
and asks the operator to accept it first. The notice says MWEB is a privacy feature, rules differ
by country, the operator is responsible, and the developers give no legal advice and no warranty.
A new notice version turns the switch off until it is accepted again. While on, Options shows
the MWEB pool of each installed Litecoin chain. Wallet code must call `mwebsvc::is_on()` before
it offers any MWEB feature.

## Wallet plan (not built)

MWEB wallets need cryptography the arcade does not have yet. In order:

1. **Keys and addresses.** Scan and spend keys from the wallet seed on Litecoin Core's path
   (`m/0'/100'/0'` scan, `m/0'/100'/1'` spend), stealth addresses `ltcmweb1...` (a scan and a
   spend pubkey per sub-address). Plain secp256k1 and tagged BLAKE3; no zkp needed.
2. **Receiving (view only).** For each extension-block output with standard fields, as libmw's
   `Keychain::RewindOutput`: shared secret `Ke * a`, view tag (first byte of a tagged hash),
   `B_i = Ko / H(OUT_KEY, t)` looked up among our sub-addresses, then the masked amount and nonce
   unmasked and the commitment re-made (Pedersen with a switch commitment: point math, no range
   proofs). Needs only the extension blocks we already store, scanned from the wallet birthday;
   spends are found by output id in later blocks' inputs. Gives MWEB balances.
3. **Peg-in** (canonical LTC into MWEB): a transaction with a witness v9 output naming the
   kernel id, plus the MWEB part (an output to ourselves, a kernel with the peg-in amount and
   the stealth excess). Needs Pedersen commitments, a Bulletproof range proof (675 bytes,
   secp256k1-zkp's bulletproof), Schnorr kernel and owner signatures, and the relay of a
   transaction with an MWEB part (today `Tx::decode_with` refuses one: it must learn to carry
   the MWEB part for the mempool and relay).
4. **MWEB to MWEB sends and peg-outs** (kernel with peg-out coins; the coins mature 6 blocks
   after the HogEx pays them).

Options for steps 3-4: bind secp256k1-zkp (the Grin/Litecoin C library; `cc` build, like
`bitcoinconsensus` in `vendor/`), or let an operator's own Litecoin Core 0.21.2+ wallet hold the
MWEB coins (the arcade only shows and asks). Steps 1-2 are a few hundred lines and safe (no
spending). Steps 3-4 move money and need an outside review before release.

All MWEB wallet features stay behind the switch above, and every UI change ships to both the
desktop app and the browser `/ui/` (one dist, built by build-all.sh).
