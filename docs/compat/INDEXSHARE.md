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

---
# Part 2 of 3: "lx" ledger copies and "dx" ord index

## 4. "lx": ledger copies (mesh/ledgershare.py + ledger_snapshot.py)
  {"t":"lx","op":"hi","nets":{net:{"h":int,"sn":null|{"id":hex64,"h":int,"n":npieces,"have":"all"|[int..]}}}}
  {"t":"lx","op":"sq","net":net}                                  (make a copy, I'm far behind)
  {"t":"lx","op":"g","net":net,"id":int,"what":"manifest","snap":sid,"z":1}
  {"t":"lx","op":"g","net":net,"id":int,"what":"piece","snap":sid,"i":int,"z":1}
  {"t":"lx","op":"gb","id":int,"seq":int,"end":bool[,"err":"busy"|"gone"][,"z":1]} + blob slice
- hi on link_up and every rebalance (:61-88); sq :163; g :166-180 (always z:1); gb :128/139/146.
- Serving (:114-149): 120 pieces/min per asker (pieces actually served; over → err "busy"); 2 serving at once.
  manifest bytes = json.dumps(m, sort_keys=True, separators=(",",":")); piece = raw 1 MiB slice; z==1 → zlib 6;
  60 KB parts, seq from 0, end on the last; an empty answer is one part b"".
- Receiving (:182-215): parts in order; total ≤ 4 MiB; inflate bounded to 4 MiB, must reach eof; requests
  forgotten after 300 s; an err pauses that peer 10 s.
- Offering: settings serve_index_copies / fetch_index_copies, default ON. want_made (:304-317): asked within
  15 min AND serving AND index not stopped AND core_tip - indexed ≤ 20 AND no fetch running; else not while
  served in the last 15 min, and only if the last copy is > 6 h old.
- make (:265-302): VACUUM INTO tmp; DELETE FROM utxo, activity, watched, covered; VACUUM; 1 MiB pieces.
- manifest = {"net","floor","activation","layout":1,"height","hash","cp","d","s","n","size","piece":1048576,
  "pieces":[sha256hex..],"at":unix}; height/hash = copy's newest block row; cp = checkpoint(height, floor).
  id = sha256(json.dumps(manifest, sort_keys=True, separators=(",",":"))).hexdigest() (Python json:
  ensure_ascii, null, ints without ".0"); the receiver RE-SERIALIZES and compares (:525): byte-exact.
- offer {"id","h","n","have":"all"} or a sorted list of held pieces while fetching; deleted after 2 h unfetched.
- Fetching (:403-496): only when (best h heard within 600 s, capped at own core tip) - own ≥ 50000; nobody
  offering a whole copy → "sq" to the top 3 holders, at most every 10 min; highest fresh whole offer with h ≤
  core tip wins; manifest re-asked every 60 s; checks id, net, layout 1, same floor and activation, piece 1 MiB,
  size ≤ len*PIECE, free disk ≥ 3*size; 6 pieces in flight at random, re-ask after 60 s; a wrong sha drops the
  offer; 600 s stall switches holder; progress saved every 15 s (<index>.part.json); dropped if own index
  passes its height first.
- VERIFY before install (:623-655): 1 every piece sha; 2 describe(copy): height, hash, cp, d, s, n all equal
  the manifest; 3 own (or borrowed) Core getblockhash(height) == hash; 4 cp and d non-null; 5 votes =
  indexcheck.votes(net, floor, cp), agreed = votes equal to d: REFUSE if agreed < 2 or agreed*2 ≤ voters (the
  maker votes like anyone). A refused id is never fetched again. Install: file swap, epoch++, record in
  <index>.copy.json.
- Confirmation (:676-792), while idle (within 20 of tip, no fetch, no TURN relaying): a shadow index built from
  own Core, 500 blocks per pass every 15 s; at shadow h ≥ cp+6 compare range_digest(floor..cp) to d; at equal
  height compare s and n. All equal → "confirmed" (shadow deleted); any difference → "differs": the self-built
  shadow REPLACES the copy; the maker is named in Admin.
- Live on Strawberry: serve:true, fetch:true; no copy made or fetched on any chain.

## 5. "dx": ord index (mesh/doginalshare.py, doginal_index.py, ord_notes.py, ord_snapshot.py)
Every frame carries "net"; frames without it (v1) are ignored.
### 5a. Frames
- hi {net, base, mode, walk, helped, v:3, rules[, sn3][, sites][, sites_mb][, sites_pages]}: walk =
  walker.next_height() if mode=="walk" else None; offer under "sn3" (LAYOUT 3); on link_up and every rebalance
  unless mode "off". base = STARTS[net]: doge 4600000, main 186920, test 34237, bel 22470, lky 88554, wjk 1,
  dgb 0, flop 18295. rules = rules_for(net): default 4, doge 5, bel 7, ltc/ltctest 6. A hi with base != own base
  is ignored; every ls/ld/dn from a peer is IGNORED unless that peer's hi.rules == own rules.
- ls {net, c, at, until}: I hold chunk c's lease, read to `at`, until now+600 (clamped). On link_up for every
  own lane, on taking a lane, every 10 blocks. Tie: if I hold the same c and (my at < theirs, or equal and their
  node_id < mine), I drop it.
- ld {net, c}: lease let go.
- dn {net, rows:[[c, hash, births(, nd)]..]}: chunk words. Sender: all own words on link_up, 500 rows/frame,
  new ones by rowid > said_upto. Receiver: ≤ 1000 rows; hash 64 hex; nd absent or 64 hex; then drops that
  node's lease on c.
- q {net, id, question:{what,...}} → a {id, <key>:..., walked_to} | {id, err≤200}: 120 answers/min per peer
  ("too many questions this minute"); asker waits 15 s; only mode "walk" answers (else "this arcade does not
  walk this chain"). what{args}: where{id}, token{tick}, number{number|id}, ticks{prefix}, holders{tick,page},
  kinds{ctype,proto}, typed{ctype,after}, balances{owner}, name{name}, on{txid,vout}, numbers{ids≤500},
  note{id}, ticklist{} (≤5000), mesh{name}, names_on{coins≤3000}, mesh_directory{all}, sites_search{q≤200,page,
  per}, moves{chunks≤64} → {str(c): moves_digest|null}; else "no such question".
- ng {net, c, id} → nb {id, seq, end, nd[, err:"gone"]} + slice: spend notes; blob = ord_notes.pack (zlib 6),
  60 KB parts, 4 sends at once; receiver total ≤ 64 MiB, parts in order.
- sq {net}: please make a snapshot; to the furthest walker with v ≥ 3 when ≥ 20000 blocks ahead, nothing
  offered and may_fetch passes; at most every 10 min.
- g {net, id, what:"manifest"|"piece", snap[, i], z:1} → gb {id, seq, end[, err:"gone"][, z:1]} + slice: same
  scheme as lx; raw parts capped at 8 MiB, inflate at 16 MiB; no "busy" (4-at-once semaphore).
### 5b. Roles
- Modes walk/help/read/off; setting ord_index:<net>, default "read".
  walk: whole index, walks every block to 2 under the tip, answers q. help: scans leased chunks of settled
  blocks (SETTLE 12), says dn, keeps notes; no q. read: no index; looks up via q; still sends hi and reads
  dn/ls. off: sends nothing.
- walkers(heard_within=300): mode walk, same rules, hi within 300 s, by walk height descending (snapshots,
  moves check, default ask). lookup(): walkers heard within 86400 s with walk > own, furthest first, falling
  back to own; "no such question" from an older walker handled (holders → token).
- Coverage SUB (coverage.py:213-262): holds "o:<net>" in walk mode; holds "t:<net>" if the ledger is within 20
  of tip; needs "o:<net>" in read/help, "t:<net>" if > 20 behind; keeper links to up to 3 holders.
### 5c. Chunks and the chunk hash
- CHUNK = 100; chunk_of(h) = (h-base)//100; range [base+100c, base+100c+100); ready_chunks(tip) =
  max(0, (tip-12+1-base)//100).
- Block line: h = sha256(block_hash.encode() + b"\n"); for txid, n, vin, ctype in found:
  h.update(f"{txid}i{n}:{vin}:{ctype}\n"); hexdigest. block_hash/txid display hex; ctype = content-type bytes
  as lowercase hex. found = births in block tx order, then per tx in births_of order: input 0's envelope n=0;
  on every_input chains later complete envelopes too; on Taproot chains (ltc, ltctest, bel) witness envelopes in
  input order. The delegate is NOT hashed.
- Chunk hash = sha256("".join(100 block lines in height order)); only when all 100 blocks present; births = sum
  of n.
- LIVE (ord-main, Pepecoin main, base 186920): block 186920 line
  5a10e2014ae217bdba551ff93ab46eb97e93ebd8dda0527f088bd95394820dca (n=1, block
  bf00f5fc1816728e12cf99f836859bb0fb02d869c246ef3fad225df044de220b); chunk 0 hash
  7ee5e962d53692f89ad8ba6fbde371d1346d7e58e98ea0142ea38561fe078364, births 3, nd
  938f2aee109b5cb8a4a0b15458b2c07d116ca0ce4b6b89378cbd88d0b8933a8b (equal across this node and 4+ peers).
- Block moves: sha256 over lines "B <iid> #<number> <txid>:<vout>:<off>\n", "M <iid> <txid>:<vout>:<off>\n",
  "X <iid>\n", "L <iid>\n", "U|L <iid> #<number>\n". moves_digest(c) = sha256("".join(100 block.moves hex)),
  or None if any missing.
### 5d. Agreement
AGREE = 2 identical hashes; AUDIT_MAX = 5. Chunk choice: walk's needed chunks first, then lowest chunk nobody
said or holds, then audits (fewest agreeing first, random among 16). A differing hash is only shown/warned;
nothing auto-wiped.
### 5e. Spend notes (ord_notes.py)
- nd = sha256(b"ord-notes\n" + notes); pack = zlib 6; unpack limit 256 MiB.
- Format: u8 version (2; 3 = witness on Taproot chains), varint nblocks; per block: 80-byte header, varint ntx;
  per tx: txid 32 bytes = bytes.fromhex(display hex) NOT reversed, varint nin; per input: u8 flags (1 coinbase |
  2 script | 4 witness); not coinbase: prev txid 32 + varint vout; script: varint len + bytes (input 0's big
  pushes zeroed when they can't matter, :202-236); witness: varint count + (varint len + bytes)*; outputs:
  varint nout; per output varint value + owner: [0]+20 (p2pkh) | [1]+20 (p2sh) | [2]+hash160(script) |
  [3,ver,len]+program (v3 only). varint = LEB128.
- Prefetch: 8 chunks past the walk, from offers[c % len] among peers that sent nd; re-ask after 120 s.
- Accept: digest == nd; decode gives 100 blocks each proved by its merkle root; chunk hash rebuilt == the hash
  that peer said. Otherwise (node, c) failed.
### 5f. Ord snapshots (ord_snapshot.py)
- Made only in walk mode on ≥ 8 cores, after an sq, SNAP_EVERY 6 h / SERVING 15 min / IDLE 2 h. VACUUM INTO,
  delete notes, pnote, stitched and meta keys lanes, lease, said_upto.
- manifest {"net","base","rules","layout":3,"height":walk_next,"size","piece":1048576,"pieces","at"}, id =
  sha256(compact sort_keys JSON), offered under "sn3".
- Fetch: may_fetch (≥ 8 cores or allowed per chain; disk ≥ 3*size) and h - own walk ≥ 20000, from walkers();
  6 in flight (2 on small machines).
- VERIFY: piece shas; every chunk where ≥ 2 arcades OTHER THAN THE MAKER agree must match the copy's chunk
  hash; a sample of up to 64 chunks' moves_digest must equal the first other walker answering "q moves"; none
  answering → accepted as "single" and flagged; mismatch refuses.
### 5g. "l" lanes (coreshare.py:216-221, 591-611)
- cr offer nets[net] = {"h","b","l"[,"p"]}, l = lanes_lent() = 4 if cpu_count ≥ 8 else 1: how many ord-index
  lanes (parallel chunk scans / tx-fetch batches) ONE borrower may run against this sharer's Core; borrower
  clamps 1..16, default 1. One lane per lent lane, round-robin over lenders; FETCH_BATCH 50 txs in proportion
  to lanes; a lender missing 3 times rests 5 min. Lanes read raw blocks over cr (getblock/getrawtransaction),
  proved by rawchain: chain data only.
