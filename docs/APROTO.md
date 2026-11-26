# A:// reacts, replies and tips (ops 8 to 10)

Likes, dislikes, replies and tips on **any transaction of any chain**, kept on the ARCADE chain so every arcade
counts them the same. A target can be a post, an inscription, a game's record, an app's own item written on
chain, anything with a txid. Nothing is assumed about what the target transaction is: the ledger doesn't look it
up, and it doesn't need to exist on the ARCADE chain.

Any app can use these. In an Arcade app window they're `window.arcade.social` (script `/r/social.js`). On an arcade's
own socket or API, they're the commands `a.reactions`, `a.replies`, `a.react`, `a.reply` and `a.tip`.

The other A:// ops (1 profile, 2/3 follow and unfollow, 4/5 contacts, 6 post, 7 wallet proofs) are described in
`crates/arcade-ledger/src/aproto.rs`.

## Wire format

An A:// record is an `arcd` AnyData record (version u16, type u16 = 200) whose data is:

    "A://" ‖ (1 << 4 | op) ‖ body

Its author is the transaction's sender, the key that signs it on ARCADE. Its fee joins the pot as for any record.

A **target** is:

    chain: u8 length ‖ chain key (ASCII, 1 to 12 lowercase letters and digits, e.g. "peptest", "arctest", "doge")
    txid:  32 bytes, in the order the txid is written (the 64-hex txid, hex-decoded as written)

| op | name   | body                                         | limits |
|----|--------|----------------------------------------------|--------|
| 8  | react  | target ‖ kind (u8)                           | kind 1 like, 2 unlike, 3 dislike, 4 undislike; nothing after the kind |
| 9  | reply  | target ‖ text (UTF-8)                        | 1 to 500 bytes |
| 10 | tip    | target ‖ memo (UTF-8, may be empty)          | memo at most 100 bytes; the same transaction pays the recipient(s) |

A record that breaks a rule is refused by every arcade's ledger: it's kept on the chain, but it counts for nothing.

### How they count

- **React:** each author has one state per target: none, liked or disliked. The author's newest react wins. A like
  or dislike replaces whatever was there. An unlike clears a like only, and an undislike clears a dislike only.
  Counts are the number of authors who currently like or dislike the target.
- **Reply:** every valid reply is kept, oldest first, with its author, text, block height and txid.
- **Tip:** the tip record's own transaction pays whoever the tipper chose. Every output to an address other than
  the sender's counts as tipped to that address; change back to the sender doesn't. A tip record whose transaction
  pays no one else is refused. Tips are counted per target: total sats, number of tips, and per recipient address.
  Readers also say which recipient is the target's **author** when an author can be found (see below). A tip to
  someone else still counts in the target's total.

### The target's author

The author of a target is the key that signed its transaction's first input, written as an ARCADE address. One
key is the same person on every chain, so a Pepecoin testnet post's author has an ARCADE address with the same key
hash. Readers find it by reading the target transaction (on the arcade's own copy of its chain, or from an arcade
that keeps it). When it can't be found (the chain isn't readable, or the first input is a script, not a key),
`author` is `null`; tips still count, per recipient.

## Activation

Ops 8 to 10 are known from a height on each ARCADE network, the same on every arcade that runs a release that has
them. Below that height they're refused as unknown ops, as releases without them refuse them anyway.

| network | from block |
|---------|------------|
| ARCADE testnet (`arctest`) | not yet set: named in the release that turns them on |
| closed test networks (`--arc-local`) | `ARCADE_A_SOCIAL_FROM` (default 0) |

Until then, `a.react`, `a.reply` and `a.tip` refuse with the height they're known from.

## Reads

Reads come from the ARCADE chain's ledger: the arcade's own copy, or, when it borrows ARCADE or hasn't caught up,
an arcade that keeps it (`explorer.a_reactions` and `explorer.a_replies` are forwarded like every ledger read).

### `a.reactions {targets, me?}`

`targets`: 1 to 100 `{chain, txid}`. `me`: `true` for this arcade's wallet, or an ARCADE address.

```json
{"cmd": "a.reactions", "targets": [{"chain": "peptest", "txid": "9f2c…e1"}], "me": true}
```
```json
{"targets": [{
  "chain": "peptest", "txid": "9f2c…e1",
  "likes": 12, "dislikes": 1, "replies": 3,
  "tips": {"total": 2500000, "count": 4, "to_author": 2000000,
           "to": [{"address": "aRiSc8…", "sats": 2000000, "count": 3, "author": true},
                  {"address": "aXeV2c…", "sats": 500000, "count": 1, "author": false}]},
  "author": {"address": "aRiSc8…"},
  "mine": {"react": "like", "tipped": 1000000}
}]}
```

`mine` is `null` without `me`. `react` is `"like"`, `"dislike"` or `null`. `tipped` is the total sats this wallet
tipped on the target.

### `a.replies {chain, txid, offset?, limit?}`

Oldest first. `limit` 1 to 200 (default 50).

```json
{"cmd": "a.replies", "chain": "peptest", "txid": "9f2c…e1", "offset": 0, "limit": 20}
```
```json
{"chain": "peptest", "txid": "9f2c…e1", "total": 3, "offset": 0,
 "replies": [{"txid": "51ab…", "author": {"address": "aXeV2c…", "tag": "shoe"}, "text": "great one", "height": 2104}]}
```

## Writes

Each write is one transaction on ARCADE, from the wallet's key there (the one holding its @tag), paid from its
ARCADE coins. Each answers `{txid, fee, ...}` as soon as the transaction is sent; it counts once it's in a block.

| command | arguments | notes |
|---------|-----------|-------|
| `a.react` | `chain, txid, kind` | `kind`: `like`, `unlike`, `dislike`, `undislike` |
| `a.reply` | `chain, txid, text` | 1 to 500 bytes |
| `a.tip` | `chain, txid, amount, to?, memo?` | `amount` in sats of ARCADE (at least the dust limit). `to`: an ARCADE address; default the target's author. When no author is found, `to` is needed. `memo` up to 100 bytes. |
| `a.follow` / `a.unfollow` | `tag` | A:// ops 2 and 3. tag may name its chain (@tags are per chain): `alice.arctest` is ARCADE's @alice; another chain's (`alice.doge`) is followed by its holder's key hash, as op 2/3 allow |

## In an app: `window.arcade.social`

Load `/r/social.js` in the app page. It asks the Arcade window that framed the page.

```html
<script src="/r/social.js"></script>
<script>
  const post = { chain: "peptest", txid: "9f2c…e1" };
  const [r] = (await arcade.social.reactions([post])).targets;
  console.log(r.likes, r.replies, r.tips.total, r.mine.react);
  await arcade.social.react(post, "like");
  await arcade.social.reply(post, "great one");
  await arcade.social.tip(post, 1000000, { memo: "thanks" });   // to the author; or { to: "a…" }
  const page = await arcade.social.replies(post, { offset: 0, limit: 20 });
  await arcade.social.follow("shoe");
</script>
```

| call | answers |
|------|---------|
| `reactions(targets)` | as `a.reactions`, with `mine` for this arcade's wallet |
| `replies(target, {offset, limit})` | as `a.replies` |
| `react(target, kind)` | `{txid, fee}` |
| `reply(target, text)` | `{txid, fee}` |
| `tip(target, amount, {to, memo})` | `{txid, fee, to, amount}` |
| `follow(tag)`, `unfollow(tag)` | `{txid, fee}` |

Reads are open to every app. Writes need the app's **Feed** switch on (on its card in the App Store, "Posts in your
name"), at most one every 0.4 s. Every tip is shown to the user on its own card first, which the app can't see or
press. A declined tip rejects with `declined`.

## For other front ends (a website, a bot)

Programs on the arcade's computer use the same commands over its socket or local API. The ledger's own reads are
also public page-API GETs, the same on every arcade that keeps the ARCADE chain (no author lookup; tips per
recipient only):

    GET /r/explorer/a_reactions?args={"chain":"arctest","targets":[{"chain":"peptest","txid":"9f2c…e1"}]}
    GET /r/explorer/a_replies?args={"chain":"arctest","target":{"chain":"peptest","txid":"9f2c…e1"},"limit":20}
 A website that serves apps
can offer `arcade.social` the same way: the reads as public GETs, and the writes signed in the reader's own
browser (the record's payload as above, in an ARCADE transaction from the reader's key).

# A:// app records (op 11)

One generic record any app can keep its own data in, on the ARCADE chain. The arcade gives the data no meaning:
an app names itself and a kind, and writes up to 1,000 opaque bytes. Every valid record is kept (none replaces
another), so an app reads them all back and decides itself what counts: the newest per author, a sum, a vote.

    body: app  (u8 length ‖ 1 to 16 bytes of a-z, 0-9, _ and -)
          kind (u8 length ‖ the same)
          data (the rest: 0 to 1,000 bytes)

| op | name       | body                    | limits |
|----|------------|-------------------------|--------|
| 11 | app record | app ‖ kind ‖ data       | app and kind 1 to 16 bytes of `a-z 0-9 _ -`; data at most 1,000 bytes |

Its author is the transaction's sender, as for every A:// record. A record whose app and kind are 13 bytes together
or less fits one OP_RETURN output (Class C, 1,024 bytes); a longer one goes out as Class B and costs more.

Op 11 is known from its **own** height on each network (not tied to ops 8 to 10); below it it's refused as an unknown
op, as releases without it refuse it anyway.

| network | from block |
|---------|------------|
| ARCADE testnet (`arctest`) | not yet set: named in the release that turns it on |
| closed test networks (`--arc-local`) | `ARCADE_A_APP_FROM` (default 0) |

## Write: `a.record {app, kind, data_hex}`

Signed by the wallet's key on ARCADE (its @tag's), or `"as": "@tag"` for another key held here, as every `a.*`
write; `client_ref` as every send. Answers `{txid, fee, app, kind, bytes}`. Until op 11 is known it refuses with
the height it's known from.

## Read: `a.records {app, kind, since?, author?, limit?, next?}`

The records of one (app, kind), oldest first (block order, then position in the block): from height `since` on,
only `author`'s (`@tag` or ARCADE address) when given, a page of `limit` (100, at most 500). `next` is the cursor of
the page after, `null` on the last page.

    {"chain": "arctest", "app": "models", "kind": "rating", "indexed": 2410, "next": "0000096a00000003",
     "records": [{"height": 2410, "pos": 3, "txid": "…", "author": {"address": "…", "tag": "alice"}, "data_hex": "7b2276…"}]}

It reads the ARCADE chain's ledger here, or (borrowed, or not caught up) an arcade that keeps it
(`explorer.a_records {chain, app, kind, since?, author? (an address), limit?, next?}`, forwarded like every ledger
read, also a public page-API GET):

    GET /r/explorer/a_records?args={"chain":"arctest","app":"models","kind":"rating","since":2400}
