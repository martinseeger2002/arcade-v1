# Index bootstrap and sharing — PLAN (not built)

Status: **plan for Martin's approval.** Nothing here is built yet. Drafted 2026-11-09 on the GX10.

## Martin's decisions

These were relayed by the JB20B session (martin-a7) from Martin's multiple-choice answers:

1. **Two sources.**
   - **Old arcades:** read their snapshots. The ord index comes over "dx" `sq`/`g`/`gb` (manifest layout 3, `ord-<net>.sqlite` in 1 MiB pieces) and the ledger over "lx" (layout 1). Each is converted **once** into our mdbx tables. SQLite is only an import format and is never kept; the no-SQLite storage rule stands. First target: Strawberry's complete Dogecoin ord index (base 4,600,000).
   - **V1.0 arcades:** share native mdbx index copies with each other, in pieces the way "cb" copies work.
2. **Scope:** every table family in our database can be bootstrapped and shared.
3. **Trust:** use a copy once its chunk hashes or digests match what **2 or more other arcades** say. Then rebuild it from our own stored blocks in the background; if it differs, replace the copy and name its maker. This is "trusted" now and "verified" later, as with the coin set after a cb copy.

References:
- the old wire format, verify rules and chunk-hash definitions: [`compat/INDEXSHARE.md`](compat/INDEXSHARE.md);
- piece transfer: [`compat/COREBOOT.md`](compat/COREBOOT.md), `crates/arcaded/src/coreboot.rs`, `cbimport.rs`;
- index code: `crates/arcade-ledger/src/ord.rs` (`OrdIndexer`, `RIDERS`), `engine.rs` (ledger), `index.rs` (tags, profiles), `crates/arcade-chaindb/src/addrbuild.rs`, `db.rs` `backfill()`.

## 1. Table families: what can be copied, from whom

| Family | Our tables (`C` = chain no.) | From old arcades | From V1.0 arcades | Checked against |
|---|---|---|---|---|
| **ord** | `ord.v1.insc num at loc block state ids pending pending_body pending_txs` | yes, dx snapshot (`ord-<net>.sqlite`) | yes, native | dx chunk hashes (100-block lines), `moves` digests |
| ord rider **tokens** (PRC-20/DRC-20) | `tok.v1.tick bal holder xfer` | yes, same snapshot (`tok bal xfer`) | yes | no old digest exists; new V1.0 `tokens` digest (§5) |
| ord rider **names** | `names.v1.claim win` | yes, same snapshot (`names`) | yes | new V1.0 `names` digest |
| ord rider **maps** | `maps.v1.map` | **no**: old arcades don't index maps; derived locally after import (§3.4) | yes | new V1.0 `maps` digest |
| **ledger** (tokens, MetaDEx, INSC, @names) | `ledger.v1.*` (property, balance, book_order, take_trade, inscription, insc_chunk, insc_piece, insc_by_number, nft_offer, nft_ask, collection_item, rows, meta, tag, tag_by_address) | yes, lx snapshot (`<net>-ledger.sqlite`) | yes | ix `d` (block fingerprints to the checkpoint), `s` (consensus hash), `n` (names digest), which we already compute byte for byte |
| **profiles** | `msg.v1.profile linked` | no (old: separate messaging store, not shared) | yes | new V1.0 digest |
| **messages** index (public posts, acts, files) | `msg.v1.cand post post_tx file gchunk act act_to machine` | no | yes | new V1.0 digest |
| **addresses** | `core.v2.script_hist by_script spends` (+ `core.v1.addr_log` while building) | **no**: old arcades strip `utxo/activity/watched/covered` from every copy | yes | new V1.0 digest per checkpoint |

Not shared: core chain tables (headers, blocks, coins: that's what cb copies and the P2P/mesh sync are for), the mempool, wallet/app data (`arcade.mdbx`), and the old arcade's working tables (`seen notes pnote stitched undo`).

## 2. Wire

### 2.1 With old arcades: match their frames exactly
Field by field, in compat/INDEXSHARE.md:
- **dx** (ord):
  - We send `hi {net, base, mode:"read", walk:null, helped:0, v:3, rules}`, so walkers treat us as a peer and list us. `rules` must equal theirs (doge 5, Pepecoin default 4), or every frame from us is ignored.
  - We read their `hi` (`sn3` offer: `{id, h, n, have}`) and `dn` rows (chunk hashes), and send `sq` to the furthest walker with v ≥ 3 when it is ≥ 20,000 blocks ahead and nothing is offered.
  - We fetch with `g {net, id, what:"manifest"|"piece", snap, i, z:1}` → `gb {id, seq, end[, err][, z]}` + 60 KB slices, zlib 6 when `z:1`.
  - Limits: raw parts ≤ 8 MiB, inflate ≤ 16 MiB; a request times out after 300 s; a holder that answers `busy` is paused 10 s.
- **lx** (ledger): `hi {nets:{net:{h, sn}}}`, `sq`, `g`/`gb` the same way. Manifest layout 1, with `d s n cp height hash`.
- **ix**: we already read `tip`. We add answering `q what:"d"` and `rows`, so old arcades can vote on our ledger too.
- Manifest ids: the receiver re-serializes the parsed manifest as Python `json.dumps(sort_keys=True, separators=(",",":"))`, ensure_ascii, and hashes it. Our serializer must match byte for byte. We only ever receive old manifests, but the check must use the same bytes.

### 2.2 Between V1.0 arcades: new frame `vx`
A new frame on direct links and relays, modeled on cb:
- `hi {nets:{net:{ix:{name:{h, cp, digest}}, sn:{name:{id, h, bytes, n}}}}}`: index tips and digests, and the copies held.
- `sq {net, index}`: please make a copy.
- `g {id, net, index, snap, what:"manifest"|"piece", i}` → `gb {id, seq, end[, err]}` + slices.

Pieces are 8 MiB (cb's `PIECE`), every piece sha256 in the manifest, manifest id = sha256 of its canonical JSON (our own serializer). Old arcades ignore an unknown `t`, so `vx` is safe on mixed links.

## 3. Converting old snapshots (once, then deleted)

The pieces are written to `<home>/import/<net>-<id>/`. When all are in and their shas match, the file is read with **rusqlite (bundled, behind a cargo feature `oldimport`, read-only)** and streamed into a **staging** area (§4.2). Then the SQLite file and pieces are deleted. Nothing SQLite is kept.

### 3.1 ord (`ord-<net>.sqlite`, layout 3) → `ord.v1.*`
`iid` = genesis txid (internal byte order) | n u32. Old `tx` BLOBs hold the txid; byte order is to be confirmed on a fixture (§8.1).

| Old table.column | Ours |
|---|---|
| `insc.tx, n` | key `ord.v1.insc C\|iid`; `ord.v1.ids` |
| `insc.number` | `ord.v1.num C\|number → iid`; `ids` value |
| `insc.height` | `h` |
| `insc.txpos` | `p` |
| `insc.ct` → `ctypes.ctype` | `ct` (the string) |
| `insc.proto, size, pieces` | `proto, size, pieces` |
| `insc.burned, delegate, parent, reinscribed` | `burned?, delegate?, parent?, re?` |
| *(none)* | **`txs`** (the txid of each piece). Old arcades don't store it. Single-piece: `txs = [genesis]`. Multi-piece: filled by a background pass that follows the spend chain from the genesis tx through our stored blocks (`SPENDS`). Until then the content of those inscriptions comes from the old arcade's `dx q` or is "not ready". |
| `at.rid→(tx,vout,off,height)` | `ord.v1.at C\|iid → txid\|vout\|off`, and `ord.v1.loc C\|txid\|vout\|off\|iid` |
| `pending.txid, ctype, left, body, size, pieces, height, origin` | `ord.v1.pending` JSON `{ct, left, size, pieces, origin, txs}`, `pending_body` (body split per piece is old-format dependent; see §8.1), `pending_txs` from the spend chain |
| `block.height, line, n, moves` | `ord.v1.block C\|height → line sha256 \| moves sha256 \| n` |
| `meta.next_number` | `ord.v1.state C\|"next"` |
| `meta.walk_next` | indexed_to("ord") = walk_next − 1 |
| `tok.tick, shown, id, number, supply, lim, dec, minted, deployer, height` | `tok.v1.tick` `{shown, id, n, supply, lim, dec, minted, deployer, h}` |
| `bal.owner, tick, overall, transferable` | `tok.v1.bal C\|owner\|tick` and `tok.v1.holder C\|tick\|owner` `{o, t}` |
| `xfer.rid→iid, tick, amt, owner, reveal` | `tok.v1.xfer C\|iid` `{tick, amt, owner, reveal}` |
| `names.rid→iid, number, height, kind, raw, spec, json, doge2023` | `names.v1.claim C\|iid` `{n, h, kind, raw, spec?, json?, doge2023?}`; `names.v1.win` recomputed from the claims (lowest number per rule and name) |
| `charm.rid, bits` | not kept: no V1.0 table. Charms are recomputed when we add them. |
| `undo, seen, notes, pnote, stitched, meta.lease/lanes/said_upto` | not imported (old working state) |

- **Rules version:** the old `meta.rules` must equal our `rules_for(net)`, and `meta.token_rules`/`token_rules_from` must match our token rules; otherwise the import is refused.
- **3.4 maps:** after the ord import, a local pass reads the bodies of `text/plain` inscriptions of 1 to 40 bytes from our stored blocks and fills `maps.v1.map` (the same rules as the walk).

### 3.2 ledger (`<net>-ledger.sqlite`, lx layout 1) → `ledger.v1.*`
Our records are the old columns under the same names. engine.rs writes them that way.

| Old | Ours |
|---|---|
| `property.*` | `ledger.v1.property C\|pid` `{id, ecosystem, type, issuer, category, subcategory, name, url, data, managed, total, creation_txid, creation_block}`; `property_by_block` |
| `balance.address, property_id, balance, selloffer_reserve, accept_reserve, metadex_reserve` | `ledger.v1.balance C\|address\|00\|pid` → 4 × u64 BE |
| `book_order.*` | `ledger.v1.book_order C\|txid` `{txid, block_height, position, address, sale_property, sale_amount, want_property, want_amount, reserved}` |
| `take_trade.*` | `ledger.v1.take_trade` `{txid, property_id, tokens, coins, maker}` |
| `inscription.*` (no `content`) | `ledger.v1.inscription` `{txid, number, creator, owner, block_height, position, content_type, content_len, sha256, json, chunks}`; `insc_by_number` |
| `inscription_chunk.*` | `ledger.v1.insc_chunk C\|sender\|00\|id\|countdown` `{txid, h, pos, body}` |
| `inscription_piece.*` | `ledger.v1.insc_piece C\|inscription\|countdown → txid` |
| `nft_offer.*`, `nft_ask.*`, `collection_item.*` | `ledger.v1.nft_offer`, `nft_ask`, `collection_item` (same fields) |
| `tag.tag, address, claimed_txid, block_height, position` | `ledger.v1.tag` `{a, t, h, p}`; `tag_by_address` |
| `arcade_tx`, `inscription_move`, `pair_trade`, `property` (by creation block) | `ledger.v1.rows C\|height`: the digest records per block (`table\|cols…\n`), exactly as index_digest builds them. That is what makes our `d` match theirs after import. |
| `meta.*` | `ledger.v1.meta` (the counters engine.rs keeps) |
| `activation` | from our chain params, not imported |
| `utxo, activity, watched, covered, dd_*, trade_*, undo, block` | not imported (private, or our own) |

### 3.3 Before staging is accepted
The importer recomputes from the staged rows what the manifest claims:
- ord: every block line, the chunk hashes, `next`;
- ledger: `d` to `cp`, `s` and `n`.

Any difference refuses the copy, and that maker is not tried again.

## 4. Taking a copy in

### 4.1 When
- An index is ≥ 20,000 blocks behind (ord: from its `base`), and a copy is offered at or below **our own stored tip**.
- The copy's block hash at its height must equal ours at that height (`HEIGHT` table). Copies are taken at a **settled** height (the 100-block checkpoint, 6 deep, as ix), because a copy has no undo records (§4.4).
- Prerequisite: the chain's blocks are stored here to at least the copy height, since we re-check and continue from our own blocks. For Dogecoin this means the doge chain installed in full mode first, or at least its blocks to 6.45 M.

### 4.2 Staging, then a switch
Rows go into the live tables under the chain's number, with that index **stopped** and marked `copying` in `STATE` (`<name>.copy = {id, h, from, state}`):
1. Clear the index's rows for that chain (as `chains.index_remove` does).
2. Write in key order, in batches (mdbx write transactions of ~64 MiB).
3. Set `indexed_to(name) = h`, then mark it `trusted`.

A crash mid-way leaves `copying`; the next start clears and restarts from the saved pieces. The "staging" is the cleared index plus that flag, so we don't need a second copy of a 50 GB table.

### 4.3 Resume
Pieces and a progress file (`have` bitmap) live in `<home>/import/…` (old format) or `<home>/copies/in/…` (native), so a fetch continues after a restart, as cbimport does. A maker silent for 900 s is let go and others with the same id are asked. The 6 pieces in flight are spread over every holder of that id.

### 4.4 Reorgs
Copied rows have no undo record below the copy height. A reorg reaching at or below that height marks the index `rebuild` and walks it from its start (or takes another copy). Taking copies only at settled checkpoints makes that practically never happen.

## 5. Checks before use ("trusted")

- **Old-format ord:** the old rule (ord_snapshot.check). For every chunk where ≥ 2 arcades **other than the maker** said the same hash (dx `dn` rows, which we now collect), our recomputed chunk hash must equal it. Then a sample of up to 64 chunks' `moves` digests must match another walker's `dx q moves`. Martin's rule adds: **at least 2 agreeing arcades besides the maker overall**, so the old "single" acceptance is not used.
- **Old-format ledger:** `d` to `cp`, `s` and `n` recomputed from the copy must equal the manifest, and ≥ 2 voters' `d(floor..cp)` must agree with a strict majority (indexcheck.votes).
- **Native copies:** each index gets a V1.0 **digest at 100-block checkpoints**, announced in `vx hi`:
  - ord: the dx chunk hashes plus `moves`;
  - tokens: sha256 over `tok.v1.tick` and `tok.v1.bal` rows in key order at `cp`;
  - names, maps, profiles, messages: sha256 over the family's rows in key order at `cp`;
  - addresses: sha256 of `script_hist` rows with height ≤ `cp` in key order, plus a running count of `by_script`.

  A native copy is used when ≥ 2 V1.0 arcades other than the maker announce the same digest at the copy's `cp`.
- **Portability:**
  - Native rows must not depend on local block numbers. `core.v2.by_script` and `spends` are keyed by our internal block number `B`, so the export writes `height|position` instead and the import maps back through our `HEIGHT` table. Every other family is keyed by txids, heights and names, and copies as is.
  - Chain numbers (`C`) are fixed per chain in `params`, so keys carry over.

## 6. Background re-check ("verified")

As with the coin set after a cb copy:
1. When idle (index at tip, no fetch running), rebuild the index into a **shadow file** `<home>/verify-<net>-<index>.mdbx` from our own stored blocks, 500 blocks a pass. Resumable, low priority, paused while syncing.
2. At each checkpoint, compare the shadow's digest with the copy's.
3. If all agree up to the copy height, the copy becomes `verified` and the shadow is deleted.
4. On any difference: mark it `differs`, keep rebuilding to the copy height, then **swap the shadow's rows in** (same switch as §4.2), name the maker in status and events, and never take a copy from that maker again.

For Dogecoin ord (180 M inscriptions) this re-check is days of background work. That's expected, and the copy is usable meanwhile.

## 7. Offering our own copies

- Made on `vx sq` (or a schedule while serving), at most every 6 h per index, from **one mdbx read transaction**, which is a consistent snapshot with no downtime. Rows are streamed per table in key order into 8 MiB pieces (zlib 6), and the manifest is written last.
- A copy is kept in `<home>/copies/out/`; it is deleted 2 h after it was last fetched, and only one is kept per index.
- Limits: 240 pieces a minute per asker, 6 served at once (Strawberry's raised limits); refused while our own disk would fall under 2× the copy size.
- We don't offer old-format (SQLite) copies to old arcades: making SQLite would break the no-SQLite rule.

## 8. Testing order

1. **Fixtures:**
   - a small `ord-test.sqlite` and `test-ledger.sqlite` from the old arcade on the GX10 (read-only copies), converted;
   - our recomputed block lines, chunk hashes and `d/s/n` must equal the old file's;
   - unit tests for every column in §3, byte orders included.
2. **Pepecoin testnet ledger over lx** from an old arcade, into a fresh V1.0 home. Our `d/s/n` at the copy height must equal both the manifest and the V1.0 index we already have. The re-check runs to `verified`.
3. **Pepecoin mainnet ord** (Strawberry walks it, 100%) over dx, into a fresh home. Compare against the mini PC's own walked ord index (its oracle).
4. **Native `vx` copies** of the same indexes between the GX10 and the mini PC, then the address index (the `B` remapping).
5. **Dogecoin ord from Strawberry:**
   - Strawberry must `VACUUM INTO` a ~35 GB snapshot; it has 83 GB free today, which is tight.
   - Transfer ~35 GB, convert on the target, then the `txs` and maps passes, then the multi-day re-check.

## 9. UI and CLI

- **Status:** `chains.status` gains, per index, `copy: {state: offered|fetching|converting|trusted|verifying|verified|differs|rebuilt, from, maker_tag, height, pct, agree}`. The Chains & Coins tab shows one line, for example "ord: copied from @strawberry at block 6,450,000 · trusted (3 arcades agree) · verifying 12%".
- **CLI:**
  - `arcade chain index copy status [key]`
  - `arcade chain index copy start <key> <index> [--from <node>]`
  - `arcade chain index copy cancel <key> <index>`
  - `arcade chain index copy verify <key> <index>`
  - `arcade chain index copy offer <key> <index> on|off`
- **API:** `chains.index_copy_status|start|cancel|verify|offer` (admin, except status).

## 10. Disk and limits

| | Dogecoin ord (today) |
|---|---|
| Download (pieces) | ~35 GB (deleted after conversion) |
| Our tables | estimate 40 to 60 GB |
| Peak during import | pieces + tables ≈ 100 GB; refused under 2.5× the manifest size free |
| Verify shadow | up to the tables' size again, while re-checking |

## 11. Phases (each one a PR)

1. **Converter and fixtures:** §3 and §8.1, no network.
2. **lx client** (ledger) with checks and the switch: §4, §5, §8.2.
3. **dx client** (hi, dn, sq, g/gb) for ord: §8.3.
4. **Re-check shadow** (§6), then the `txs`/maps passes.
5. **Native `vx`:** offer and fetch, digests per family, address remap (§8.4).
6. **Dogecoin from Strawberry** (§8.5).

## Open questions for Martin

1. While the Dogecoin copy is in progress, should Dogecoin ord lookups be **answered by asking a walker live** (dx `q`, as old "read" arcades do)? That's small to build and gives @arcade_v1 Dogecoin data right away.
2. Strawberry has 83 GB free for a ~35 GB snapshot next to its 35 GB index and 56 GB work files. OK to make it there, or should space be cleared first?
3. Should charms (old `charm` table) become a V1.0 table now, so the import keeps them?
