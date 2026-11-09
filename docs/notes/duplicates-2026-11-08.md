# Duplicate-code survey (2026-11-08)

Read-only survey of the workspace. Paths are relative to crates/. Item A (canonical JSON / DEL) is already
fixed: link::canonical now delegates to canon::canonical (commit 9454678).

## Inconsistencies (fix first, each with a test)
- B. Randomness helpers that ignore read errors and continue with zeros (`let _ = File::open("/dev/urandom")...`):
  transport.rs:98 rand_bytes; realtime.rs:661 token() (session tokens); realtime.rs:225 (guest ids);
  crsource.rs:34 (an Ed25519 seed). arcade-app/src/lib.rs:150 random and arcade-wallet/src/keys.rs:30 getrandom
  panic instead.
- C. Address → script done three ways: api.rs:348-350 treats any non-P2SH version as P2PKH (accepts other
  chains' addresses); borrow.rs:100 and explorer.rs:571 return None for unknown versions. Sites skipping the
  version check: walletsvc.rs:148/435/455, feedsvc.rs:295, api.rs:112, msgsvc.rs:645 (walletsvc.rs:435 is safe
  only because resolve_to, walletsvc.rs:328, checks first).
- D. Ping replies differ: meshsvc.rs:326 and bk.rs:49 cut "n" to ≤16 chars; crsource.rs:121, shares.rs:527,
  borrow.rs:387 echo the raw value.
- E. crsource.rs:142 inflates zlib answers without a size cap; coreshare.rs:34 inflate caps at MAX_ASKED.
- F. JS: PEP() hardcodes " PEPtest" (apps.js:20, feed.js:20, messages.js:26, appwindow.js:51; app.js:15 pep fixed
  to "peptest"): wrong on a mainnet home chain. COLORS in chains.js:15 lacks doge (explorer.js:11 has it).
- G. Coin amounts through f64: msgsvc.rs:528 coins_text, msgsvc.rs:689 (fee). walletsvc.rs:86, explorer.rs:259
  use integer maths. rpc.rs:39 f64 is Core-compatible and fine.

## Ranked duplication
1. (done) Canonical JSON.
2. Ed25519 "sign JSON minus fields" (3 copies): realtime.rs:134/142/148 (strip sig/hops/d, + blob, key "o");
   udpsvc.rs:946/955/960 (strip sig/hops/t, key "from"); welcome.rs:14/20/27 (strip sig, key "a"). Home:
   arcade_mesh::canon::{sign_json, verify_json}(v, strip, key_field, blob). Each caller passes exactly its
   current parameters (byte-exact).
3. Random bytes/tokens (~8 ways): arcade-app lib.rs:150 random, wallet keys.rs:30 getrandom, transport.rs:96,
   realtime.rs:225, realtime.rs:659, crsource.rs:34; OsRng fills in udpsvc.rs:152 rand_hex, http.rs:48,
   apikeys.rs:45-46, walletsvc.rs:416/602, envelope.rs:130, public.rs:225; rand::random in stun.rs:14,
   udp.rs:484. One random<N>() + rand_hex(n) on OsRng. Fixes B.
4. Coreshare result unwrap: CoreShareSource::r (crsource.rs:47) = borrow::r (borrow.rs:412); move next to
   ask. crsource.rs:140 zlib duplicates coreshare::inflate (:34) without the cap: reuse it (fixes E).
5. Link dial/ask wrappers with a ping loop (5 copies): meshsource.rs:25/31 → bk.rs:42; shares.rs:516;
   crsource.rs:30; borrow.rs:336 + drain :377; meshsvc.rs:166/216. Moderately risky: keep each caller's identity
   (node / borrower-derived / throwaway), timeouts and limits. One ping-reply rule (fixes D).
6. Address → script (~10 sites): borrow::script_of (borrow.rs:100) = explorer::script_for (explorer.rs:571);
   inline at api.rs:348-350, api.rs:112, walletsvc.rs:148/435/455, feedsvc.rs:295, msgsvc.rs:645,
   faucet.rs:116/123, miner.rs:95; P2SH script built by hand in borrow.rs:106, explorer.rs:577, api.rs:350,
   importsvc.rs:33. Home: arcade_ledger::address::{p2sh_script, script_for_address(p, a) -> Option}. Decide per
   site whether P2SH is allowed; wallet spend paths stay P2PKH-only. Fixes C.
7. Script classification (3 copies): ledger script.rs:48 classify; chaindb schema.rs:83 script_id; db.rs:98
   compress_script (+ expand_script :120). P2PK rules differ (compress_script requires 02/03). RISKY: script_id
   and the compressed format are on-disk keys: share only predicates, outputs byte-exact.
8. hash160/sha256d: arcade_chain::hash (hash.rs:46/51/57) is the original; ledger address.rs:8 and wallet
   keys.rs:163 are copies. Re-export.
9. Base58Check/bech32: ledger address.rs:12/19 the home; near-copies digidollar.rs:126 and wallet keys.rs:125
   bs58_check; bech32 private in importsvc.rs:190 → move to ledger address.
10. Messaging-key fingerprint: arcade_msg keys.rs:29 = ledger index.rs:236; grpsvc.rs:26 gid same hash without
    spaces. Keep the user-visible format.
11. Secretbox base64(nonce24||box): grpsvc.rs:401/414 seal_live/open_live and accountsvc.rs:313 open_mailbox.
    Keep each KDF at the caller (byte-exact); share the framing.
12. Blake2b with personalisation: udp.rs:41 blake2b = link.rs:61 blake2b32; arcaded uses the blake2 crate in
    realtime.rs:49 room_id and accountsvc.rs:317. Keep one helper; outputs unchanged.
13. Node-id hex (12 inline copies: udpsvc.rs:141, realtime.rs:232/208, shares.rs:57, meshsvc.rs:77/112,
    faucet.rs:10/28/86, announce.rs:45, api.rs:60, main.rs:176) → Node::id_hex().
14. JS api()/$/invoke (8 copies: app.js:5, apitab.js:5, apps.js:6, chains.js:5, explorer.js:6, feed.js:6,
    messages.js:5, appwindow.js:19). app.js loads first (index.html:282); appwindow.html needs its own or a
    common.js.
15. JS h() builder (apps.js:11, feed.js:11, messages.js:10, explorer.js:35).
16. JS coin formatting and tickers: TICKER app.js:12, TICKERS chains.js:12 = explorer.js:13; coins app.js:14,
    coinAmt chains.js:41, coins explorer.js:46, PEP/pep (F); COIN_ICONS/ICON_FILE chains.js:13-14 =
    explorer.js:14-15; logoOf chains.js:42 ≈ coinEl explorer.js:17; COLORS (F).
17. JS ago/short: apps.js:21, feed.js:21, explorer.js:47, messages.js:19 when; short app.js:17 vs explorer.js:45;
    inline slices messages.js:24/103.
18. JS status setters: msg app.js:18, apitab.js:18, chains.js:38/48, apps.js:25, feed.js:24, messages.js:30.
19. Token amount formatting: walletsvc.rs:72 shown, explorer.rs:259 token_amount, walletsvc.rs:69 divisible vs
    explorer.rs:260, msgsvc.rs:528 (G), JS units explorer.js:53. One fmt_units in ledger.
20. Token property/balance reads: walletsvc.rs:65 prop vs explorer.rs:304/313; holdings :93 vs explorer.rs:315-319;
    ledger_count explorer.rs:253. Accessors in ledger engine.
21. ~60 inline from_be_bytes decodes (engine.rs, explorer.rs, ord.rs, msgsvc.rs, walletsvc.rs) → be_u32/be_u64;
    explorer row builders explorer.rs:187/208 vs :468/475.
22. Home-dir resolution: arcaded main.rs:46, arcade-cli main.rs:45, arcade-desktop main.rs:8 (desktop doesn't
    check arcade.mdbx); socket client: cli main.rs:61, cli main.rs:290, desktop main.rs:25.
23. Script push/varints: wallet tx.rs:33 push vs miner.rs:27 (≤75 bytes only); wallet keys.rs:186 varstr vs
    chain encode.rs:119 write_varint.
24. Small wrappers: now() (accountsvc.rs:21, meshsvc.rs:21, udpsvc.rs:148, realtime.rs:39 vs arcade_app::now);
    is_hex (udpsvc.rs:158, http.rs:258, appsvc.rs:374, explorer.rs:733/736, cards.rs:41, public.rs:314;
    realtime.rs:57 is_guest lowercase-only); short-id (udpsvc.rs:144, borrow.rs:1273, psync.rs:379, faucet.rs:22);
    e() explorer.rs:25 = borrow.rs:32; c(node) feedsvc.rs:20 = walletsvc.rs:15; faucet.rs:12 duplicates
    welcome::frame (welcome.rs:44); accountsvc.rs:164/178 host normalisation.

## Not worth consolidating
STUN encode/decode is single (arcade_mesh::stun); the BlockSource impls are different transports; secp256k1
sign_message (DER, sha256d) vs signmessage (Core compact) are different formats (note: verify_message
normalizes S, so it accepts high-S; verifymessage ignores the address version).
