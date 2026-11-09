# Index sharing in the old arcade — what V1.0 must match

From the Arcade v1.0 session's read-only survey of ~/.dogecoinarcade/src/arcade (2026-11-08); part 1 of 3
(sections 0-3). Parts 2-3 (lx / dx / kc details) are appended below when they arrive.

## 0. Overview: two index families and their frame kinds
A. Token/name LEDGER index (LedgerIndex, <net>-ledger.sqlite, web/state.py:1716,1773): arcade_tx,
   properties/tokens, balances, MetaDEx (book_order), pair_trade, arcade INSC inscriptions + moves,
   nft_offer/nft_ask, @names (table `tag`). On every chain in state.token_chains (state.py:1488).
   - "ix": index check (fingerprint tips, range digests, bisection). mesh/indexcheck.py.
   - "lx": whole-index copies in pieces. mesh/ledgershare.py + ledger_snapshot.py.
   - HTTP GET /bootstrap/<net>.sqlite.gz and .json (manual clone; web/app.py:32660, bootstrap.py).
B. ORD index (DoginalIndex, ord-<net>.sqlite, doginal_index.py): Doginals/ord inscriptions + owners + moves;
   DRC-20/PRC-20 "-20" tokens (tok/bal/xfer); dns names (`names`, the .mesh web); charms.
   - "dx": chunk leases, chunk hashes, spend notes, remote queries, snapshots. mesh/doginalshare.py.
C. Related: "kc" known collections (mesh/collectionshare.py). "cr" carries NO index ops (no arcade_ord_*); only
   arcade_utxoset and arcade_coinscan (coreshare.py:56-67); its "l" sets ord-index lanes. SUB "res" keys
   o:<net> / t:<net> say which index a node holds (mesh/coverage.py).
- Relayed links carry only cr, dx, lx, ix (relay.py:40-41); kc direct links only.
- MAX_FRAME 64 KB; blob senders slice at PART = 60 KB (48 KB for cr). rebalance() every 60 s.
- Not flooded: link by link; the link proves the sender.

## 1. What is shared
1a. Ord inscriptions via "dx": (i) chunk hashes ("dn") checked by audit; (ii) spend notes ("ng"/"nb");
    (iii) snapshots ("sq"/"g"/"gb", whole ord-<net>.sqlite in 1 MiB pieces); (iv) remote queries ("q"/"a").
1b. @names (ledger `tag`): indexed by each arcade; shared only via "ix" n (names_digest) and "lx" copies.
    dns/.mesh names (ord `names`): "dx" q, what = name|mesh|mesh_directory|names_on (doginal_index.py:3114-3127),
    and in ord snapshots. Block maps (bitmap/dogemap/pepemap): NOT indexed (ord_rules.is_block_map drops them).
1c. -20 tokens (ord tok/bal): "dx" q, what = token|ticks|holders|balances|ticklist (doginal_index.py:3099-3121),
    and in ord snapshots; chunk hashes don't cover token state; no balance digest. No ord/-20 index on ltc,
    ltctest, kot. Arcade properties, balances, MetaDEx: "ix" (d + s) and "lx".
1d. Tags/ledger/MetaDEx: as 1c second bullet.
1e. Messaging: no message index shared; the messaging chain's LEDGER index gets ix/lx like any other.
1f. Address/coin scans: NOT shared (utxo, activity, watched, covered are private, stripped from copies). Coin
    data lent only via cr: arcade_utxoset (Kotia only; params [{"watch":[..],"addresses":[..]}], ≤200 addresses,
    ≤5000 watched; answer {"height","best","watching","coins":{addr:[[txid,vout,value,height]..]},
    "history":[[txid,address,height,received,spent,paid_out,other]..]}) and arcade_coinscan (params
    [{"addresses":[..≤100]}]; answer {"coins":{addr:[[txid,vout,sats,height]..]},"height":{addr:h},
    "waiting":[..],"next":secs}; one scantxoutset per chain per 600 s; borrower once per 60 s, ≤5 parts per
    batch; -32601 = not lent).
1g. Known collections: "kc".

## 2. "ix": index check (ledger), mesh/indexcheck.py
  {"t":"ix","op":"tip","nets":{net:{"h":int,"at":int,"d":hex64[,"s":hex64][,"n":hex64]}}}
  {"t":"ix","op":"q","id":int,"net":str,"what":"d","lo":int,"hi":int}
  {"t":"ix","op":"q","id":int,"net":str,"what":"rows","h":int}
  {"t":"ix","op":"a","id":int[,"d":hex64][,"rows":[str..]][,"err":str<=120]}
- tip on link_up (:65) and on rebalance only when changed (:85). a (:130): d = digest(lo,hi), or rows =
  [r[:600] for r in rows[:200]]; err = str(exc)[:120]. "a" resolves only when id matches AND same peer.
- Tip fields (index_digest.py:123-142): h = indexed height (newest ledger `block` row); at = (h-6)//100*100,
  or None if below floor (then no tip sent); d = digest(floor, at); s = consensus_hash; n = names_digest; s
  and n included only if h unchanged after computing them. floor = chain.params.index_start.
- Limits: 120 questions/min per peer (else err "not answered"); one at a time; asker waits 30 s; digest raises
  "that range is not settled here yet" if hi > own checkpoint.
- Compare (:173-210): at = min(mine.at, theirs.at); use the d of whichever side has at == that; ask the other
  "q d lo=floor hi=at" (the ASKER's floor). Equal → agree_upto = at. Differ → bisect: while lo<hi: mid =
  (lo+hi)//2; digest(lo,mid) differs → hi=mid else lo=mid+1; then "rows" at that block, for Admin. At equal h
  also compare s and n. No automatic action on mismatch (shown in Admin → Chains).
- votes(net, floor, cp): {peer: d(floor..cp)} from reachable peers with tip at >= cp (tip.d if at == cp, else
  ask); non-answers not counted. The quorum for lx copies.
- Live (Strawberry): test at 1562200 9/9 agree; main at 1285400 9/9; doge at 6449800 9/9; dgb 8/8, bel 8/8,
  flop 6/6, wjk 8/8.

## 3. Exact ledger digests
3a. Block fingerprint (index_digest.py:30-83). Tables and columns, in gathering order:
    arcade_tx        txid,position,encoding_class,message_type,message_version,sender,reference,payload_hex,valid
    inscription      txid,number,creator,position,content_type,content_len,sha256,chunks
    inscription_move txid,inscription,from_address,to_address,position,how
    pair_trade       txid,seq,position,taker,maker,maker_txid,gave_property,gave,got_property,got
    nft_offer        txid,position,inscription,buyer,take_kind,take_property,take_amount
    nft_ask          txid,position,inscription,seller,take_kind,take_property,take_amount
    property (keyed by creation_block) property_id,ecosystem,property_type,issuer,category,subcategory,name,url,data,managed,creation_txid
  A missing table is skipped. NOT covered: invalid_reason, inscription owner/json/content, tag, balance,
  book_order (those are in s and n). Record = table + "|" + "|".join("" if v is None else str(v)) + "\n"
  (integers decimal). Per height ALL records from all tables in one list, list.sort() (plain string sort =
  UTF-8 byte order, "\n" included); block digest = sha256("".join(sorted).encode()).hexdigest(). Heights with
  no records have no digest.
3b. Range digest (:86-91): h = sha256(f"{lo}-{hi}\n"); for height in sorted(heights with a digest, lo<=x<=hi):
    h.update(f"{height}:{digest}\n"); hexdigest. Inclusive both ends; empty range = sha256("<lo>-<hi>\n").
3c. Checkpoint: EVERY=100, SETTLED=6, at = (indexed-6)//100*100, None if < floor; cache resets on reorg /
    rebuild / index file replaced.
3d. consensus_hash s (consensushash.py:206-217): ONE sha256 over the ASCII records of all sections in order, no
    separators:
    - balances "address|propertyid|balance|selloffer_reserve|accept_reserve|metadex_reserve" ORDER BY address,
      property_id; rows with all four buckets 0 skipped (:64-81);
    - dex_offers "txid|seller|property_id|offer_amount|native_desired|min_fee|time_limit" ORDER BY txid (if table);
    - dex_accepts "matched_txid|buyer|amount|amount_remaining|accept_block" ORDER BY matched_txid, buyer (if table);
    - metadex "txid|address|sale_property|sale_amount|want_property|want_amount|reserved" FROM book_order WHERE
      sale_amount > 0 ORDER BY txid (:145-155);
    - crowdsales: empty;
    - properties "property_id|issuer", ecosystem 1 then 2, ORDER BY property_id (:176-184).
    SQLite BINARY collation; .encode("ascii") (non-ASCII raises).
3e. names_digest n (ledger_snapshot.py:100-109): SELECT tag, address, block_height FROM tag ORDER BY tag;
    h.update(f"{tag}|{address}|{block_height}\n"). Missing table → empty digest e3b0c442…b855.
3f. Live examples (recomputed from Strawberry's files):
  - wjk: floor 228000, at 236600, no records. d = 02434c4735783f87af2667631789daf7cb3b3e8f90439f202e17abab945b5e5d
    = sha256(b"228000-236600\n"); s = n = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.
  - main: floor 1208000, at 1285400. d = f24d81e73cd238f3874e52e3ede585609ab4dc5b376fe0775b7d9c7675ac3872; s = n = empty.
    Block 1249128, one record:
    "arcade_tx|0d8d0ef79d8dc93c9c15702768be16dcb848fdcc34efeed0ca513559eb875336|2|C|200|0|Pjy1S2Qn26PTzysPHxL2Ut3fMzSZhvWpne|Pk3TeQ6imXhzB8n8CeHSQGAKanPxFufc7S|000000c86172636d010707dd03cc092a5163f550060af713a29df49be6aada56f8908272d103add1ac3bc3|1\n"
    → block digest 62547163aaef966886744eeb44b66045395e4970e5e5cd05aadeca9482d31189;
    range_digest(1249128..1249128): preimage "1249128-1249128\n1249128:62547163…1189\n" →
    23cdba40c00d0fcba1d67acd255581850250530142f92886f29cfaba3b99fa6d.
  - test: floor 1496133, at 1562200, 10906 blocks with records.
    d = dd4f9a4170121de9f78f2e6da4f61dcfa7a41dbc122f16e40f63de16e27c4c50
    s = 035d1d727b0197c13afe232883682c9f2b849abe7adddd7fbd1f688504e75038
    n = b10500e8fb61e90250de1903aee98e044c9d24667a4d7d874a0c4b1052348e96
    Block 1496134 (6 records) digest 6d5561600ab4ea1a6e8f408a32d76892e0a2d50117fcbcd41988c966a428b3fe;
    range_digest(1496134..1496134) = 49dad94ed23aad45e5a0c40e57269279cb04da0b597775b8f1570540de461833.
3g. TRAP: bootstrap.py:327-363 has a SECOND, different "index_digest" for the HTTP bootstrap manifest (stale
    column names → sections silently skipped). Never use it for "ix".
