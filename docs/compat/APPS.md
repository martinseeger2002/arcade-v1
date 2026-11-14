# App cards (the original's "game cards") — what V1.0 must match

From the Arcade v1.0 session's read-only reading of the original (games.py, inscriptions.py,
launchlist.py, cardwatch.py, web/app.py 26932-27331, web/content.py 57-95, web/templates/games.html,
base.html 332-770 and 1852, web/docs/games.md), 2026-11-08. V1.0 calls them **app cards**.

## 1. What a card is
- The JSON field of an ordinary **arcade INSC inscription** (not ord/doginals) whose content type
  starts with `text/html`. INSC: AnyData 200, Class B, chunks "INSC" ver 1 kind 1 · id8 · countdown u16 ·
  clen u16; the first chunk carries the manifest (total u32 · sha256 32 · ctype_len u8 · ctype ·
  json_len u16 · json, JSON ≤ 65,535 bytes).
- Top-level key `"game"` or `"app"` (if both, `"game"` wins). Values cleaned, never trusted:
  - text (str/int/float, whitespace collapsed, cut): name 60 (**required**), description 280,
    version 20, players 20, genre 24;
  - booleans only when exactly `true`: multiplayer, wallet, media, pictures, notify, contacts, groups;
  - family `^[A-Za-z0-9._:-]{1,64}$` else ""; cover, play, tdeck `^[0-9a-f]{64}$` (lowercased) else "";
    tdeck_icon `[a-z]{1,16}`;
  - minimized `{"timeout": n}` or n: absent/invalid 900, 0 none, else clamp 1..86400 (→ minimized_timeout).
- Author = the inscription's **creator** address (not the owner). No url, icon (cover is the icon),
  category (genre) or chains fields.
- **V1.0 adds `category`** (the original ignores it): lowercase letters, digits and inner dashes, up
  to 24 ("Node Modules" reads as "node-modules"). The App Store shows its known categories first
  (games, apps, social, node-modules: `appsvc::CATEGORIES`), then any other a card names, so a new
  kind of app needs no new arcade. Without one, a "game" card is in games and an "app" card in apps.

## 2. Versions and removal
- One card per game: key (creator, name casefolded), newest by (height, position).
- `play`: a card-only inscription may point at another page; used only if that page has the same creator.
- Feed acts (likes, dislikes, comments, tips) on any older version (same creator + name, same chain)
  count for the shown card; each person's last word across versions counts.
- Removal: a FEED_ACT DELETE (kind 6) on the card txid by the creator (or their announcement-bound
  other-chain address). Checked on the newest card: a removed newest card takes the game off the list
  (an older version doesn't come back); a newer card restores it. The inscription still plays at its link.

## 3. Listing
- Every chain's ledger index; HTML inscriptions whose JSON mentions "game"/"app", parsed (V1.0: parse
  before any limit). Same card on two chains shows twice, with a chain badge.
- score = likes − dislikes + 0.5·comments (+ trade terms, always 0 for cards); Popular = hot(score,
  block time); New = time desc, mempool first. Starred apps first.
- Card watch (per node): flags a card whose name/family looks like an earlier card by another creator
  (spelling(): NFKD, accents off, rn→m vv→w cl→d, lookalikes 0→o 1/i/j/!/|→l 3→e 4/@→a 5/$→s 7→t 8→b
  6/9→g 2→z + Cyrillic/Greek, keep [a-z0-9], collapse repeats; parts < 4 ignored; equal, containment
  (≥5), Levenshtein ≤1 (len<7) or ≤2, or the same cover); trusted publishers never flagged; an optional
  model decides. Flagged cards are hidden until the operator clears them.
- Shown per card: cover (or name tile), name, star, pills (multiplayer, genre, chain), "by @tag · when ·
  vX · N players · tipped N", description, Open, permission switches for what it claims, like/dislike/
  comment/tip, Remove (if mine), Details, last 3 comments, Discuss.

- **V1.0 star ratings live in reviews.** A review is an ordinary comment (FEED_ACT REPLY on the card,
  any version) whose text begins with five marks, ★ for each star and ☆ for the rest, stars first
  ("★★★★☆ works well" = 4). The original shows it as a comment. Each person's latest review with
  stars counts; an EDIT by its writer changes it, a DELETE by its writer takes it back. Shown: average,
  count, 1–5 spread. Popularity adds (stars − 3) / 2 per rater (5 stars +1, 1 star −1) to the score.

## 4. Launch and sandbox
- Open runs `/content/<opens>` in an iframe (opens = play if same creator, else the card's page).
- /content CSP: `sandbox allow-scripts allow-pointer-lock; default-src 'self' data: blob:; script-src
  'self' 'unsafe-inline' 'unsafe-eval' data: blob:; style-src 'self' 'unsafe-inline' data:; img-src
  'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; connect-src 'self';
  frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'` (+ explicit origin beside
  'self' for Safari), ACAO *, nosniff, immutable cache. Opaque origin, no cookies, fetches only to the node.
- Identity: a viewer ticket ?v= per (address, network, page), checked against the Referer's /content/<txid>.
- Callbacks: /r/storage.js, /r/realtime.js (rooms keyed by family), /r/app.js, /r/access.js,
  /r/contacts.js, /r/groups.js, /r/llm.js, /r/node.js, /r/swap.js, /r/owner.js, /r/state.js (type 11),
  /r/escrow.js, /r/wallet, /r/inscriptions?creator=…; wallet actions via
  `parent.postMessage({arcade: "take"|"claim"|"inscribe"|...})`, behind the gate: a card whose switch
  is on passes; off asks once (kept per page); no card always asks naming the publisher; unknown asks every time.

## 5. Vectors (Pepecoin testnet)
- Riffs 2.0 7092dfd7…8277 (creator nWKJgSeA…X1Gk) replaces Riffs 1.0 b1580e0e…5b31.
  `{"app":{"name":"Riffs","description":"Group chats with text channels, …","version":"2.0","players":"2-20",
  "multiplayer":true,"family":"riffs","genre":"Social","media":true,"groups":true,"notify":true,"pictures":true,
  "cover":"c1eaa01180715c7829185dc8020a34153045bae05c912874b231bb24934ff32c"}}`
- Browser 1.3 ae5803de…8dcc (text/html;charset=utf-8; cover, tdeck, tdeck_icon "globe"); older 1.2.1 9054923a…938e.
- Pocket Tide card-only 5d7b8c27…c26b with play ef715c2b…8638 (same creator).
- Tank Simulator (three versions: 986ff97a…a16f, 11007dad…560e, e209554d…1d11) plays c3da3315…25c4.
- LiteWorldsQuest Beta b4cc67e9…cffd6 ("game" key).
- Parse: `{"app":{"name":"  A   b "}}` → "A b", 900; minimized 0 → none; {"timeout":999999} → 86400;
  multiplayer "true" → false; name true → not a card; game and app → game; upper-case cover accepted;
  family "has space" → "".
