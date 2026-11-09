# "cb" Core-data copies (coreboot) in the old arcade — what V1.0 must match

From the Arcade v1.0 session's read-only survey (2026-11-08). Section A is its first message (the wire); section B its follow-up.

## A. Protocol

cb (coreboot) Core-data copy protocol, read-only from arcade/mesh/coreboot.py + arcade/core_copy.py at c69f944e0. Nothing changed.

0. WHAT IT IS, AND A CATCH FOR DOGE
- A copy is a snapshot of blocks/, chainstate/ and indexes/, cut into 8 MiB pieces, with a sha256 per piece in a manifest. It is NOT a stream of blocks. The old fetcher installs it whole and only uses it when it is ≥20,000 blocks behind (WORTH_BLOCKS).
- V1.0 can take only blocks/blk*.dat if it parses them itself. Dogecoin 1.14 blk files are not XOR-obfuscated, but they do hold AuxPoW headers. Records are magic+len+block in on-disk order, not height order.
- WHO OFFERS A NET: only nets in CoreBoot.nets(). That means either (a) the setting core_installed:<net> exists (installed from Coins & chains) and mode core:<net> == "here", or (b) a systemd user unit: SERVICE_UNITS is only {"test": pepecoind-testnet.service, "main": pepecoind-mainnet.service}.
- The arcade must also have share:<net> (default True), a synced Core (blocks ≥ headers−2, verificationprogress ≥ 0.999, not pruned) and txindex=1 in its conf. So "doge" is offered only by an arcade that installed Dogecoin Core through Coins & chains, runs it "here" and has txindex. Check each linked arcade's cb hi to see whether any does.

1. FRAMES (t:"cb", over a DIRECT link only)
- hi {nets:{net:{h,ver,gen,can,sn}}}. Sent on link_up, again after a copy is made (rebalance), and once a minute via rebalance per the docstring.
  - h = getblockchaininfo.blocks.
  - ver = getnetworkinfo.version as an int (e.g. 1140700).
  - gen = getblockhash(0).
  - can = synced AND conf txindex=1, i.e. "able to MAKE a copy if you sq me".
  - sn = the copy it holds now, or null: {id, h, bytes, n, maker, hash}. id = the manifest id (below). h = copy height. bytes = total. n = total number of pieces. maker = the maker's mesh node_id. hash = the tip block hash.
  - A net is left out of hi when its Core is down or share is off.
  - The receiver keeps hi for HEARD_FRESH = 600 s.
- sq {net}: "I'm far behind, please make a copy". There is no answer frame. The server records asked[net][peer] = now, valid for 15 min.
  - On its next 1 s tick it makes a copy if all of these hold: someone asked; share on; synced; no copy younger than SNAP_EVERY = 6 h (fresh()); not already making; txindex; ≥2 GB free.
  - When done it sends a new hi with sn filled. Making means minutes of hashing; the Core is down only for seconds.
  - The old fetcher re-sends sq at most every 10 min (ASK_EVERY).
- g {id:int, net, snap:<sn.id>, what:"manifest"|"piece", f?, i?}
  - what "manifest": f and i are absent.
  - what "piece": f = index into manifest.files[] (int), i = piece index within that file (int). Piece i covers bytes [i·8 MiB, min((i+1)·8 MiB, n)).
  - The server silently ignores a g whose id is not an int, whose snap is not a str, whose net is not in its nets(), or whose net has share off. No reply is sent at all.
- gb {id, seq, end[, err][, z]} + blob
  - Data is cut into PART = 60 KiB slices; MAX_FRAME is 64 KiB.
  - seq runs 0,1,2… consecutively; end:true on the last slice. A zero-length payload still sends one empty slice with end:true.
  - Manifest: zlib.compress(level 6) of the WHOLE canonical JSON, then sliced. z:1 is set on every slice. Inflate the concatenation, not each slice.
  - Pieces: raw bytes, no compression, no z.
  - err (always with seq 0, end true): "busy" = the per-asker rate limit was hit (the old fetcher pauses that holder 10 s). "gone" = the server has no such snap/manifest/piece, or the file changed since it was hashed, in which case the server DROPS the whole copy. No other err values exist.
  - There is NO checksum in gb. Check sha256(whole piece) == manifest.files[f].s[i] and len == the expected size.
  - The receiver drops the request on a seq gap or an oversize total (manifest ≤ 64 MiB, piece ≤ 8 MiB). It times out a waiting request after 300 s.

2. MANIFEST (canonical JSON: sort_keys, separators (",",":"))
{v:1, net, genesis, version:int, subversion:str, height:int, hash:<tip>, utxo:{height, bestblock, txouts, total_amount, hash_serialized_*|muhash (whatever the Core returns)}, txindex:bool, maker:<node_id>, piece:8388608, bytes:int, files:[{p:"blocks/blk00000.dat", n:<size>, s:[sha256 hex per 8 MiB piece…]}, …]}
- sn.id = manifest_id = sha256(canonical JSON of the manifest).hex. The receiver must recompute it; Fetch.begin refuses a mismatch. The old receiver also requires manifest.hash == sn.hash and manifest.height == sn.h, and txindex true.
- Files: every regular file under blocks/ (blk*.dat, rev*.dat, blocks/index/* LevelDB), chainstate/* and indexes/* (txindex etc., whole), walked in sorted order (dirs sorted, names sorted).
- Skipped: LOCK, LOG, LOG.old, .lock, anything whose path part starts with "wallet", and symlinks.
- No per-file sha256: only per-piece. A file's last piece is shorter.
- The old receiver refuses paths that are absolute, contain "..", or whose first part is not blocks/chainstate/indexes.

3. "DIRECT"
- on_frame does `if node.links.get(link.peer_id) is not link: return`. So only the TCP/WS link in node.links counts. A RelayLink (rl) or anything else is dropped silently.
- Nothing is required of the asker: no version check, no allowlist, no prior hi. A g just needs a valid snap id, which you learn from hi.
- Limits are per server:
  - SERVING_AT_ONCE = 2 g's served concurrently across ALL askers (a semaphore; extra ones wait, they are not refused).
  - PIECES_PER_MINUTE = 60 per asker peer_id, a sliding 60 s window. Over that you get err "busy". That is ≈ 8 MiB/s per asker per holder.
  - Manifest requests don't count toward the limit.
- The old fetcher keeps IN_FLIGHT = 4 pieces per holder and treats a piece as lost after 120 s. It spreads across all holders, torrent-style: holders = linked peers whose hi sn.id == the same id. Every arcade that installed a copy re-serves it (core_copy.seed), with the same id.
- The old fetcher only uses offers whose gen AND ver equal its own Core's. A server doesn't check that about you, but match ver if you ever want to hand the copy to a Core.

4. SNAPSHOT CONSISTENCY (core_copy.take)
- Order: refuse if pruned or not synced; setnetworkactive false; gettxoutsetinfo; verify getbestblockhash == utxo.bestblock (else abort and re-enable the network); Core `stop` (waits until it is down).
- Then into <chaindir>.arcade-snapshot/work/data: HARD-LINK files that never change (LevelDB .ldb/.sst; blk/rev files numbered below newest−1), COPY the rest (LevelDB logs/MANIFEST/CURRENT, the newest two blk/rev numbers). Then the Core is started again.
- So blk files, block index, chainstate and txindex agree exactly at manifest.height. Downtime is seconds; hashing runs afterwards with the Core up.
- The folder is renamed to <chaindir>.arcade-snapshot/<id>/ beside manifest.json and stat.json ({path:[size, mtime_ns]}, used to detect changes → "gone").
- Age: no new copy while one is < 6 h old. A copy nobody fetched from for 2 h (SNAP_IDLE, measured from the last piece served) is deleted and disappears from hi. Only one copy is kept per net.
- sn naming: id = manifest_id (64 hex). There is no date in the name; height and the tip are in sn.
- The receiver's own checks before trusting a copy:
  - the copy tip is on its own header chain at that height (getchaintips → walk back);
  - after the swap, a Core started alone (-maxconnections=0 -listen=0 -dnsseed=0 -disablewallet) has getbestblockhash == hash, gettxoutsetinfo matches the utxo fields, and verifychain(3, 288) == true.
  - The maker's UTXO set is trusted, not replayed.

5. LEDGER-INDEX BOOTSTRAP OVER HTTP (bootstrap.py + web/app.py:32521), briefly
- GET /bootstrap/<net>.sqlite.gz = gzip of a `VACUUM INTO` copy of the LEDGER index (never the messaging store).
- GET /bootstrap/<net>.json = {network, height, floor, consensus_hash, index_digest, sha256 (of the .gz file), bytes, made (unix), file, what}.
- Header X-Arcade-Height. 503 with Retry-After: 120 while the first copy is being built; 404 if there is none.
- It is rebuilt on request when older than 3600 s or ≥100 blocks behind the tip.
- Check it by re-deriving consensus_hash and index_digest at that height.
- I haven't read the mesh lx/dx snapshot code for this message. Ask if you want those frames field-by-field.

Noted your coreshare facts: q bodies must be plain JSON [[method, params], …] (not zlib), and a second link from an already-linked node id is closed. node.py:683 confirms the second: `if link.peer_id in self.links: await link.close()`.

## B. Follow-up (2026-11-08)
- What it makes: a consistent copy of the Core datadir's blocks/ (blk*.dat + rev*.dat, blocks/index LevelDB),
  chainstate/ (LevelDB) and indexes/ (txindex; DigiByte also digidollarstats), 8 MiB pieces + sha256 manifest.
  Never LOCK, LOG, LOG.old, .lock, wallets, conf, cookie, peers. Made automatically when a linked arcade sends
  "cb" sq: setnetworkactive false, gettxoutsetinfo, bestblock == tip, stop, hard-link immutable files and copy the
  rest, restart Core, hash on a thread. Remade at most every 6 h; deleted after 2 h unfetched. Makers: Core
  installed via Coins & chains (core_installed:<net>, mode "here") or Pepecoin test/main user units; share:<net>
  on; synced; not pruned; txindex=1; ≥ 2 GB free.
- Reported sizes (Strawberry's Coins & chains): doge 249.9 GB (@pepmonster), ltc 300.1 GB, ltctest 9.6 GB, bel
  506.3 GB, lky 1.93 GB (@orange); kot 1.32 GB, dgbtest 0.60 GB (@banana); dgb 51.6 GB, flop 1.33 GB (@pumpkin).
- Distribution: mesh only ("cb" hi/sq/g/gb, direct links only); torrent-like: every arcade that installed a copy
  re-serves it under the same manifest id; fetcher spreads 4 pieces in flight per holder over every linked
  holder with that id. Resume: pieces in <chain folder>.arcade-copy/, kept across restarts; a maker silent for
  900 s is let go.
- Use: when Core ≥ 20000 blocks behind its own header chain, pick the linked holder with the highest copy and
  same Core ver and gen. Verify: (a) every piece sha256; (b) the copy's tip hash at its height == this node's OWN
  header chain at that height; (c) start alone (-maxconnections=0 -listen=0 -dnsseed=0 -disablewallet): tip ==
  manifest, gettxoutsetinfo == the manifest's utxo summary, verifychain level 3 over the last 288 blocks. NOT
  checked: that the maker's coin set is what the chain produces (would need -reindex-chainstate); the maker is
  trusted and named on Coins & chains. No signatures, no votes. Install: stop Core, move old set aside, move copy
  in, run (c), start; failure → old set back, maker not tried again.
- Today: Strawberry offers no copies. Each linked arcade's "cb" hi lists nets with h, ver, gen, can, sn (sn
  non-null = ready now, size in "bytes"; can:true = will make one on sq).
- For V1.0: blk*.dat records are [magic 4][u32 LE length][block] in on-disk ARRIVAL order (not height order), may
  have zero padding at the end; Dogecoin-family blocks carry AuxPoW headers; Dogecoin 1.14 doesn't XOR-obfuscate
  blk files (newer Bitcoin-derived Cores might: blocks/xor.dat). The old fetcher only uses offers whose ver and
  gen equal its OWN Core's (servers don't check), so advertise matching values to let old arcades fetch from V1.0.
