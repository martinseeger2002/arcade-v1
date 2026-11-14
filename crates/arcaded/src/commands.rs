//! Every command the arcade answers, with what it needs. One table behind the unix socket
//! (the app and CLI), JSON-RPC for sidecar apps (/rpc, cookie: everything) and the web API
//! (/api/v1, API keys with permissions), and docs/API.md (examples/apidocs).

/// What a command needs over the web API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Perm {
    /// Public data: anyone (still only from this computer unless the operator opens it).
    Public,
    /// Private reads: your messages, groups, contacts, wallet keys, activity.
    Read,
    /// Send and manage private messages, groups, contacts, mutes.
    Message,
    /// Public posts and feed acts (not tips).
    Post,
    /// Tips (feed acts that pay), under the key's daily limit.
    Tip,
    /// Wallet actions: tag claims, faucet, wallet links, key announcement.
    Wallet,
    /// Running the arcade: chains, indexes, mining.
    Admin,
    /// Never over the web API (the 12 words, creating a wallet): the socket and cookie RPC only.
    Local,
}

impl Perm {
    pub fn name(self) -> &'static str {
        match self {
            Perm::Public => "public",
            Perm::Read => "read",
            Perm::Message => "message",
            Perm::Post => "post",
            Perm::Tip => "tip",
            Perm::Wallet => "wallet",
            Perm::Admin => "admin",
            Perm::Local => "local",
        }
    }
    pub fn from_name(s: &str) -> Option<Perm> {
        PERMS.iter().copied().find(|p| p.name() == s)
    }
}

/// Permissions a key can be given (Public needs none; Local is never given).
pub const PERMS: &[Perm] = &[Perm::Read, Perm::Message, Perm::Post, Perm::Tip, Perm::Wallet, Perm::Admin];

pub struct Cmd {
    pub name: &'static str,
    pub perm: Perm,
    /// Parameters: "name" required, "name?" optional.
    pub params: &'static str,
    pub summary: &'static str,
}

macro_rules! cmds {
    ($($name:literal $perm:ident $params:literal $summary:literal;)*) => {
        pub static COMMANDS: &[Cmd] = &[$(Cmd { name: $name, perm: Perm::$perm, params: $params, summary: $summary },)*];
    };
}

cmds! {
    "status" Public "" "Home chain, sync state, tags, profiles, mesh links, node id.";
    "peers" Read "" "Arcades linked over the mesh right now.";
    "mesh.udp" Read "" "UDP hole punching: STUN server and proved addresses, NAT kind, which links run over UDP paths (RTT, packets), introductions.";
    "mesh.udp_set" Admin "on?, lan_dial?, stun_port?, drop?" "Turn UDP paths and introductions on or off (ARCADE_MESH_UDP=0 overrides), offer LAN addresses to every peer, set the STUN port for the next start (0 = none); drop = a peer id prefix whose path to fail now (falls back to TCP).";
    "events" Read "n?, kind?" "Recent activity (newest first), optionally of one kind.";
    "mempool" Public "" "Home chain transactions waiting for a block.";
    // For a website server in front of this arcade (its users hold their own keys)
    "tx.coins" Public "addresses, chain?" "Spendable coins of up to 500 addresses (confirmed, not spent by a waiting transaction, and unconfirmed outputs), to build a transaction for a browser to sign; mining rewards not mature yet listed apart (immature, spendable_after). Home or installed chain, Addresses index on.";
    "tx.activity" Public "addresses, since?, chain?" "What changed for up to 500 addresses: history rows above height since, and waiting transactions paying or spending them; height to pass as since next time.";
    "tx.fee" Public "chain?" "The fee rate (per 1000 bytes) and dust limit this arcade builds transactions with.";
    "swap.book" Public "" "Every open swap order of this arcade and the arcades linked to it (any leg for any leg, across chains).";
    "swap.place" Local "give, get, key?, payout?, maker_fee?" "Place a swap order (give a leg, get a leg). key {chain: secret hex} and payout {chain: address}: an order run with its own key for somebody else (a website visitor), its result paid to them; kept here, never shared. maker_fee {bps <= 500, to: {chain: address}}: the maker's operator's fee, paid in what the maker gives.";
    "swap.release" Local "order" "Give back an open order placed with its own key: cancelled, its key and payout addresses forgotten here.";
    "swap.quote" Admin "node, order, taker" "For a taker outside this arcade (a website visitor whose browser signs): ask the maker's arcade to hold its order for this taker (this arcade's name plus the visitor's reference) and give its terms, keys, lock time and whether its lock is sized.";
    "swap.told" Admin "node, order, hash, taker, taker_keys, taker_time, taker_height?, taker_lock" "For a taker outside this arcade: tell the maker's arcade the taker's lock is on the chain; it checks it on its own copy and locks its side.";
    "swap.status" Admin "node, hash" "For a taker outside this arcade: the maker's side of a swap (its step and its lock).";
    "tx.broadcast" Post "hex, chain?" "Send a transaction signed elsewhere (a website user's browser): every input must spend a coin this arcade knows, outputs no more than inputs; pooled here and sent to the coin's network.";
    // Wallet
    "wallet.create" Local "phrase?" "Create a wallet (or restore one from 12 words). Returns the new words once.";
    "wallet.phrase" Local "" "The wallet's 12 words.";
    "wallet.keys" Read "chain?" "Your addresses on a chain, their @tags and balances.";
    "dd.wallet" Read "chain?" "Your DigiDollar on DigiByte (chain dgb, the default, or dgbtest): address (DD.../TD...), cents, coins, borrowed, whole (false when only coins from the borrow floor on can be seen).";
    "dd.coins" Read "chain?, pubkey" "The DigiDollar address and coins (txid, vout, cents, height) of any DigiByte key (33-byte hex), as dd.wallet shows this wallet's: for a website whose accounts sign in their browsers.";
    "dd.send" Local "chain?, to, cents, send?" "Send DigiDollars (cents, $1 to $100,000 per send) to a DD.../TD... address, with DGB from the same wallet for the fee (0.35 DGB/kB, at least 0.1, at most 0.5 DGB); send: true broadcasts, otherwise it's only built and shown.";
    "wallet.send" Local "chain?, to, amount, from?, send?" "Pay amount (satoshis) on a chain (home, installed or borrowed) from this wallet (from: one of its addresses or an imported wallet's; otherwise the own address with the most), signed here; send: true broadcasts, otherwise it's only built and shown with its fee (and a warning when the address could be meant for another chain).";
    "wallet.link" Wallet "chain, wif, tag" "Link another wallet to your @tag (signed by both keys).";
    "wallet.unlink" Wallet "chain, address, tag" "Remove a linked wallet.";
    "wallet.links" Read "chain" "Your linked wallets on a chain.";
    "account.signin" Local "tag, password, from?" "Sign in with an existing Dogecoin Arcade account (@tag + password): its words come from the arcade that holds it (from, or any we know), sealed under the password, then its imported wallets and mailbox. The account becomes the active one.";
    "account.list" Read "" "Accounts on this arcade: its own wallet first, then signed-in accounts; which one is active.";
    "account.switch" Local "id" "Use another account (\"\" = this arcade's own wallet): wallet, messages, groups and rooms follow it.";
    "account.signout" Local "id" "Sign an account out of this arcade: its words, keys, messages and groups here are forgotten.";
    "wallet.import" Local "chain, secret, addr_kind?, name?" "Import a wallet: a WIF key, or another wallet's words (one address, index 0); addr_kind legacy (default), p2sh-p2wpkh or p2wpkh. Kept encrypted here; nothing is published.";
    "wallet.imports" Local "" "Imported wallets, with balances on chains this arcade keeps addresses for.";
    "wallet.import_remove" Local "chain, address" "Forget an imported wallet and its key.";
    "tag.check" Public "tag" "Whether a @tag is free and valid.";
    "tag.claim" Wallet "tag" "Claim a @tag on the home chain (test coins for it are asked for by themselves).";
    "start.status" Read "" "Getting started: wallet made, handle (claimed or being claimed), test coins asked for or here, synced.";
    "tag.list" Read "" "Your tag claims and their progress.";
    "faucet.ask" Wallet "index?, tag?" "Ask the mesh for test coins.";
    // Messaging
    "msg.me" Read "" "Your messaging key, fingerprint and whether it is published.";
    "msg.stats" Public "" "How many message candidates, posts, acts and machine records the home chain holds.";
    "msg.scan" Message "" "Look for new messages now (it also runs every 15 seconds).";
    "msg.who" Public "name" "Who a @tag or address is, and their published messaging key.";
    "msg.conversations" Read "requests?" "Conversations, newest first (requests: people not in your contacts).";
    "msg.thread" Read "peer" "Every message with one person (their messaging key).";
    "msg.read" Message "peer" "Mark a conversation read.";
    "msg.unread" Read "" "Unread counts: messages, requests, group lines.";
    "msg.file" Read "id" "A message's file (base64).";
    "msg.send" Message "to, text?, file?" "Send a private message to a @tag, address or key; file = {name, type, data (base64)}.";
    "msg.delete" Message "id" "Stop showing a message here (the chain keeps it).";
    "msg.announce" Wallet "" "Publish your messaging key now if it isn't on chain.";
    "contacts.list" Read "" "Your contacts.";
    "contacts.save" Message "key, name?, address?, notes?" "Add or change a contact (by messaging key).";
    "contacts.delete" Message "key" "Remove a contact.";
    "mute.list" Read "" "Muted addresses.";
    "mute.add" Message "address" "Mute an address (messages and posts).";
    "mute.remove" Message "address" "Unmute an address.";
    // Groups
    "group.list" Read "" "Your groups, with their last line and unread count.";
    "group.create" Message "name, members, description?" "Make a private group (members: @tags or addresses) and invite them.";
    "group.thread" Read "id" "A group's lines, members and extras.";
    "group.read" Message "id" "Mark a group read.";
    "group.say" Message "id, text?, file?" "Write to a group.";
    "group.leave" Message "id" "Leave a group.";
    "group.app_list" Read "" "Your groups as apps see them: id, name, live room, members.";
    "group.app_read" Read "id, after?, limit?" "A group's lines for an app (oldest first).";
    "group.seal" Read "id, text" "Seal text for a group's live room (the original's group live key).";
    "group.open" Read "id, sealed" "Open text sealed for a group's live room (null if not this group's).";
    "group.add" Message "id, members" "Add members to a group (they get its key).";
    "group.update" Message "id, changes" "Admins: change admins, description, picture or pinned message.";
    "group.remove" Message "id, members" "Admins: remove members (gives everyone else a new group key).";
    // Feed
    "feed" Public "order?, channel?, tag?, author?, txid?, limit?, offset?" "Public posts (popular, new, friends or following) with replies, counts and tips.";
    "feed.channels" Public "" "Channels and how many posts each has.";
    "feed.settings" Read "set?" "Feed settings (hide muted, contacts only); set = {hide_muted, contacts_only} needs post.";
    "feed.post" Post "text?, channel?, file?" "Post publicly (needs a @tag).";
    "feed.act" Post "kind, target, text?, amount?" "Like, unlike, dislike, undislike, reply, share, edit, delete; tip (needs tip; amount in satoshis).";
    "follows" Public "who" "An account's followers and the accounts it follows (who = @tag or address), with @tags and counts.";
    "follow" Post "who, on?" "Follow an account (on: false unfollows): a feed act on the home chain from this wallet; the newest between two accounts counts.";
    "feed.file" Public "txid" "A post's file (base64).";
    // Protocol indexes
    "ledger" Public "" "The arcade ledger's fingerprints next to what other arcades report.";
    "profile" Public "who" "A published profile (who = @tag or address).";
    "ord" Public "id?, chain?" "An inscription, or ord index counts.";
    "ord.send" Local "inscription, to, chain?, send?" "Send one inscription (id or number) from this wallet's address on a chain kept here with its ord index: its coin whole to `to` first, clean coins pay the fee. Only one alone at the start of its coin. send: true broadcasts, otherwise it is built and shown with its fee.";
    "ord.chunks" Public "from?, chain?" "Ord fingerprint chunks (as other arcades compare them).";
    "name" Public "name?, chain?" "A name claim, or counts.";
    "map" Public "map?, chain?" "A map claim, or counts.";
    "token" Public "tick?, chain?" "A token, or how many there are.";
    "tokens" Public "owner, chain?" "Token balances of an address.";
    // Explorer
    "explorer.summary" Public "chain?" "Chain summary for the explorer's front page.";
    "explorer.blocks" Public "chain?, offset?, limit?" "Recent blocks.";
    "explorer.block" Public "chain?, id" "One block (height or hash).";
    "explorer.tx" Public "chain?, txid" "One transaction.";
    "explorer.address" Public "chain?, address, history?, inscriptions?, content?" "An address: balance, history, inscriptions (the first 60; all with explorer.held), tokens. history: false or inscriptions: false leaves that part out; content: false lists inscriptions without previews.";
    "explorer.inscriptions" Public "chain?, offset?, limit?" "Inscriptions, newest first.";
    "explorer.inscription" Public "chain?, id" "One inscription.";
    "explorer.tokens" Public "chain?, q?, open?, offset?, limit?" "Tokens, in tick order: q, those starting with it; open: true, only those still being minted (minted under supply).";
    "explorer.token" Public "chain?, tick, offset?, n?" "One token and a page of its holders, biggest first (n: 25, at most 100; total: how many hold it).";
    "explorer.collections" Public "chain?, q?, sort?, status?, source?, offset?, n?" "A page of a chain's collections, on-chain (kind registry) and Nintondo's known ones (kind known; id = slug): sort newest, oldest, name, size or holders; status active or all; source all, registry or known; n at most 60. Sub 1K first.";
    "explorer.collection" Public "chain?, id | slug, offset?, n?" "One collection (a root inscription id, sub1k, or a known collection's slug) and a page of its items by number (n: 48, at most 96). A known one not held yet is asked for and answers loading {state, have, of}.";
    "explorer.collection_of" Public "chain?, id | ids" "The collections an inscription is in (ids: up to 500 at once, answered as items in order).";
    "explorer.dogefeed" Public "chain?, type?, cursor?, limit?, include_invalid?" "D:// (DogeFeed Protocol) activity, newest first, each record checked as DogeFeed's indexer guide says (valid, reason, registrar): posts, comments, quotes with their counts; likes, branches, follows, tips, reveals, deletes, keylinks, keyrevokes. type: a comma list; cursor: next from the last page. Needs the dogefeed index on Dogecoin.";
    "explorer.dogefeed_tx" Public "chain?, txid" "One D:// record by its transaction: decoded, checked, and why not when invalid.";
    "explorer.dogefeed_inscription" Public "chain?, id, activity?, type?, cursor?, limit?" "A D:// post, comment or quote: the record, counts (likes, branches, quotes, comments, tips, reveals), latest edit, deleted; activity: true (or type) lists what targets it.";
    "explorer.dogefeed_address" Public "chain?, address, direction?, type?, cursor?, limit?, counts?" "An address on D://: its counts (posts, likes given and received, followers, following, tips, linked keys) and activity (direction out: what it did; in: what paid it).";
    "explorer.dogefeed_keys" Public "chain?" "The D:// registrars this arcade checks records against: DogeFeed's, this arcade's own, and any the operator added.";
    "explorer.held" Public "chain?, address, offset?, limit?, order?" "The ord inscriptions an address holds, a page at a time by number (order old or new; limit 100, at most 500), without content: {total, items: [{id, n, h, ct, size, at, p}]}.";
    "explorer.names" Public "chain?" "Names.";
    "explorer.name" Public "chain?, name" "One name.";
    "explorer.maps" Public "chain?" "Maps.";
    "explorer.map" Public "chain?, map" "One map.";
    "explorer.mempool" Public "chain?" "Waiting transactions.";
    "explorer.ledger" Public "chain?" "Arcade ledger summary.";
    "explorer.ledger_tokens" Public "chain?, offset?, limit?" "Arcade ledger tokens.";
    "explorer.ledger_token" Public "chain?, id" "One arcade ledger token.";
    "explorer.ledger_inscriptions" Public "chain?, offset?, limit?" "Arcade ledger inscriptions.";
    "explorer.ledger_inscription" Public "chain?, id" "One arcade ledger inscription.";
    "explorer.tag" Public "chain?, tag" "A @tag's page: addresses (with linked and imported wallets) and balances.";
    "explorer.search" Public "chain?, q" "Search: block, tx, address, @tag, inscription, token, name.";
    "explorer.inscription_types" Public "chain?" "How many ord inscriptions of each kind (images, text, HTML, ...) a chain holds.";
    // Chains
    "chains.peers" Read "key" "A chain's P2P address book: nodes known, working in the last day, resting after failures; the working ones and the next to try.";
    "mesh.overview" Public "" "The arcade mesh at a glance: every arcade linked or heard lately, its operator, and per chain what it lends, the chain copies it can make (and why not), the index copies it offers, how far it walks ord, and whether its ledger fingerprint matches ours.";
    "chains.list" Public "" "Every chain V1.0 knows, installed or not, with sizes reported by arcades.";
    "chains.status" Public "key" "One chain's staged progress.";
    "chains.offers" Public "" "Chains other arcades offer over the mesh.";
    "chains.sizes" Admin "" "Measure this arcade's disk use per chain now.";
    "chains.add" Admin "key, mode?, with?" "Install a chain (mode full, shared or chain; with = indexes). One install at a time: while another chain is still being installed it waits in a queue (queued, position).";
    "chains.unqueue" Admin "key" "Take a chain out of the install queue.";
    "chains.pause" Admin "key" "Pause a chain.";
    "chains.resume" Admin "key" "Resume a chain.";
    "chains.index_add" Admin "key, index" "Add an index to a chain.";
    "chains.index_remove" Admin "key, index" "Remove an index from a chain.";
    "chains.remove" Admin "key" "Remove a chain and its data.";
    "chains.copy_status" Public "key?" "Core-data copies (cb) linked arcades offer for installed chains, the fetch and import in progress, and whether a chain's coin set is trusted from a copy or verified.";
    "chains.copy_start" Admin "key, what?" "Bootstrap a chain from the best copy offered: what = coins (the coin set; missing blocks are fetched from the network and stored, default) or all (the copy's blocks too).";
    "chains.copy_cancel" Admin "key" "Stop fetching a copy (its pieces are deleted) or decline the offer: the chain syncs the usual way.";
    "chains.refill" Admin "key, from?, to?, sources?" "Fetch back block bodies lost here (pruned, and no sealed copy answers for them) between from and to (default: the whole chain), 50 at a time: a batch is taken only when at least sources (default 2, at most 5) independent sources (different node ids and IP addresses: arcades holding the blocks, arcades lending their Core, coin P2P peers) sent the same bytes, each block matching our header, merkle root and stored txids. Runs in the background; see chains.refill_status.";
    "chains.refill_status" Public "key?" "Block refills (chains.refill): state, heights from/to/at, lost bodies found, restored, batches that couldn't be agreed, and the sources that agreed last.";
    "chains.refill_cancel" Admin "key" "Stop a block refill (what it restored stays).";
    "chains.check_rules" Admin "key, from?, to?, threads?, scripts?" "Check this arcade's stored blocks of a chain against the chain's consensus rules (the coin rules, and the scripts and block rules, where the chain has them), from..to (default the whole chain), the coins each block spends read from the blocks holding them (sealed ones included). Scripts in every block, or with scripts \"assume\" only above the newest checkpoint. Read only, in the background at the lowest priority on threads (default 2, at most 8); see chains.check_rules_status.";
    "chains.check_rules_status" Public "key?" "Rule checks (chains.check_rules): state, from/to/at and pct, blocks checked, transactions and inputs, blocks paying exactly the whole reward, blocks without a body here, and the first blocks that break the rules.";
    "chains.check_rules_cancel" Admin "key" "Stop a rule check.";
    "update.status" Public "" "Updates: this arcade's version, whether automatic updates are on (off unless the operator turns them on), the newest release, whether it is newer, when it last looked, what it is doing, and why it can't update itself if it can't (a local build, another system, a folder it can't write).";
    "update.set" Admin "on" "Turn automatic updates on or off. When on, it looks for a new release every 6 hours and installs it: checked against the release's SHA-256, the old programs kept as .prev, then a clean restart.";
    "update.check" Admin "" "Look for a new release now (installs nothing).";
    "update.now" Admin "" "Install the newest release now, if it is newer than this one, and restart.";
    "chains.rule_reports" Public "key?" "What arcades found checking each chain's rules not switched on while they connected its blocks (their first sync and every block since): this arcade's report and every one heard over the mesh, per arcade the heights checked, blocks paying the whole reward and blocks breaking them (scripts above the newest checkpoint); the broken blocks named; and the heights two or more arcades checked clean (clean_by_2). Evidence for switching a chain's rules on, not proof.";
    "chains.copy_verify" Admin "key" "Re-check a coin set taken from a copy: derive it from the blocks again (ours replaces it if it differs).";
    "chains.index_copy_status" Public "key?" "Index copies (docs/INDEXBOOT.md): what old arcades offer per chain, the fetch, its votes, and whether an index here came from a copy.";
    "chains.index_copy_start" Admin "key, index, from?" "Fetch an index copy now from the best offer (index = ledger or ord; from = one arcade's node id): it is taken in only if 2 or more other arcades agree with it.";
    "chains.index_copy_cancel" Admin "key, index" "Stop fetching an index copy (its pieces are deleted).";
    // V1.0 copies (docs/V1SHARE.md)
    "share.held" Public "" "V1.0 copies of chain parts held here (blocks, coins, ord, tokens, names, maps, ledger, profiles, messages): id, height, rows, size; and those being made.";
    "share.offers" Public "key" "V1.0 copies other arcades hold for a chain, per part: id, height, how many made it themselves and how many hold it.";
    "share.fetch" Admin "key, fam, id?" "Fetch a part's copy from every arcade holding it at once: the newest one 2 or more arcades made themselves (or the id given). Pieces are checked against the manifest; the copy is then held and served here too.";
    "share.install" Admin "key" "Install an added chain (or the home chain) from V1.0 copies: asks the linked V1.0 arcades for blocks and coins copies a few blocks ahead, waits until 2 or more made them, fetched from every holder and taken in as they come (no replay); the coins are trusted until re-checked.";
    "share.install_cancel" Admin "key" "Stop installing a chain from copies.";
    "share.cancel" Admin "key, fam" "Stop fetching a copy (its pieces are deleted).";
    "share.serve" Admin "key, on" "Answer other V1.0 arcades' requests for copies of a chain (on by default): a copy is made when this arcade reaches the asked height, and deleted after 2 hours unused.";
    "share.make" Admin "key, fam" "Make a V1.0 copy of one part of an installed chain now, from one frozen view of the database (in the background; see share.held).";
    // Borrowed coins
    "borrow.status" Read "" "Borrowed coins: the switches, and per chain its lenders, tip, floor, tip mismatch and your balance.";
    "borrow.set" Admin "on, chain?" "Turn borrowing on or off: for every coin (no chain) or for one chain (off removes its borrowed data).";
    "borrow.coins" Read "chain" "Your coins on a borrowed chain (from blocks read since the floor, and the lender's coin scan).";
    "borrow.send" Local "chain, to, amount, from?, send?" "Pay amount (satoshis) on a borrowed chain, signed here; send: true broadcasts to every lender, otherwise only built and shown.";
    // Mining
    "mine.start" Admin "threads?" "Mine the home chain: only at the tip (synced with a coin peer, no higher height known); until then it waits and mine.status says why.";
    "mine.stop" Admin "" "Stop mining.";
    "mine.status" Read "" "Mining state and hash rate; waiting: why it is not mining now (not at the tip).";
    "stake.status" Read "" "KOT staking: on or off, coins, self-check, blocks staked.";
    "stake.set" Admin "on" "Turn KOT staking on or off.";
    "stake.check" Read "" "Check our KOT stake rules against the newest blocks.";
    // Apps (app cards)
    "apps.store" Public "sort?, q?, flagged?, category?" "The App Store: app cards from every chain with the arcade ledger (popular or new; starred first), in categories (counts in \"categories\"; category = one of them).";
    "apps.categories" Public "" "The App Store's known categories (games, apps, social, node modules, themes); a card may name any other.";
    "apps.installed" Read "" "Apps installed on this arcade, each at its newest version.";
    "apps.install" Admin "chain, txid" "Install an app (it follows its author's updates).";
    "apps.uninstall" Admin "id" "Uninstall an app.";
    "apps.star" Admin "chain, txid, on?" "Star or unstar an app (starred show first).";
    "apps.card" Public "chain, txid" "One app: card, versions, comments, permission switches.";
    "apps.perms" Admin "id, set?" "An app's permission switches (wallet, contacts, groups, media, pictures, notify).";
    "apps.review" Admin "chain, txid, verdict" "Clear or confirm an app flagged as a lookalike.";
    "apps.trust" Admin "address, on?" "Trust a publisher (never flagged as a lookalike).";
    "apps.storage" Local "page, op, key?, value?" "An app page's saves (load, set, remove, clear, history), as arcade.storage.";
    "realtime.join" Local "page, room, game?, member?, cred?, fast?" "Join a room over the mesh as this arcade's player (game defaults to the page): a token, me and the members; fast: true also takes the fast lane (realtime.fast). A website joins for its user with member (their address, or a guest id) and cred {a, x, s} signed over realtime.cert's words.";
    "realtime.ice" Local "tag?, lan?" "ICE servers for one player (tag: their address or account, kept apart only for the relay's per-login limit): logins on this arcade's media relay where other arcades proved its STUN port open (lan: true adds its LAN address, for a player on its own network), else on relays borrowed from linked arcades; and this arcade's proved STUN. What a website hands its pages for WebRTC.";
    "realtime.cert" Public "address" "The words an address signs (Core signmessage) to play through this arcade, and their expiry: a website's user signs them in the browser and the site passes {a: address, x: expires, s: signature} to realtime.join.";
    "realtime.send" Local "token, data?, b64?" "Send to everyone in the room: text or base64 bytes, at most 512 bytes, about 5 a second.";
    "realtime.leave" Local "token" "Leave the room.";
    "realtime.poll" Local "token, wait?" "Room events for this seat (join, leave, message), waiting up to wait ms (15000 at most); a seat not polled for 30 s is let go.";
    "realtime.fast" Local "token, to, data?, b64?, reliable?" "Send to some players of the room over the fast lane between arcades (a seat joined with fast): text or base64 bytes, at most 16 KB, about 240 a second; to names the players (up to 64). Reaches those who play here or through an arcade with a lane up (realtime.lanes); unreliable sends waiting to go out are replaced by the newest, reliable ones keep their order. Heard as fast events.";
    "realtime.media" Local "token, to, b64" "A piece of the app window's encoded camera, microphone or screen (up to 60 KB) for some players of the room: over the direct link to their arcade, else through at most two arcades on the room's route that have their media relay on; dropped when a link is behind. Heard as mchunk events (the window's own, never the page's).";
    "trade.leg" Local "give, take, buyer" "A player trade (arcade.swap.trade), the side that signs: build this wallet's leg giving `give` for `take` (each {inscription}, {token, amount} or {coins}), sign it SINGLE|ANYONECANPAY and seal it to the buyer's messaging key: {sealed, fee, give, take}. Its two coins are held back 30 minutes.";
    "trade.open" Local "sealed" "Open a trade sealed to this player: {leg} or {bundle}.";
    "trade.fill" Local "leg, expect, peer, send?" "Check a leg from `peer` against the trade agreed (`expect`, the signer's view {give, take}: the swap it names, its seller, each signature, its coins unspent, what it pays) and complete it with this wallet's coins: the fee; with send, signed and sent: {txid}.";
    "trade.watch" Local "txid, inscription?, owner?" "How a sent trade stands: settled (in a block, and the inscription held by owner there), failed, or waiting.";
    "trade.items" Local "" "What this player holds, for a trade window: {address, inscriptions, balances}.";
    "trade.who" Local "address" "Whether a player can be sealed to (has published a messaging key).";
    "prize.seed" Local "page, pool?, game?, session?, judge?, referee?" "A seed for one run (the seed door): from the referee of a pool (the page's own unless it names one), of the page's refereed pools (game), or of a judge, issued to this arcade's player: {seed, expires[, pools]}.";
    "prize.claim" Local "page, secret, pool?, lot?, replay?, send?" "Claim one free lot of a prize pool on the chain (the claim door): the page's own token pool unless it names one, opened with its phrase; a refereed pool's referee checks the run (replay). Without send: what it costs ({piece, price, fee, pool_pays}); with send: signed and sent, {txid, piece}.";
    "collections.known_get" Read "" "Whether this arcade asks Nintondo for its known collections: {known}.";
    "dogefeed.stamp" Post "record" "This arcade's registrar stamp (att) for a D:// record its author signed in their browser: checked as DogeFeed's guide checks it, then attested with this arcade's kid. Answers the stamped record, the fee address and the 0.1 DOGE fee its reveal must pay.";
    "dogefeed.sign_action" Post "op, target, inputs, recipient?, fee" "This arcade's registrar signature for a paid D:// action a browser builds (like, branch, follow, tip, reveal, delete, keyrevoke): the 80-byte OP_RETURN data, its fee output paying this arcade's fee address.";
    "dogefeed.post" Wallet "type?, text?, media?, reply?, quote?, pay?, ref?, tags?, mentions?, send?" "Post, comment, quote or edit on D:// from this arcade's own Dogecoin wallet: signed, stamped by this arcade and inscribed as DogeFeed writes them. Without send: the record and its cost.";
    "dogefeed.act" Wallet "op, target, amount?, send?" "Like, branch, follow, tip, reveal, delete or keyrevoke on D:// from this arcade's own Dogecoin wallet (1 DOGE in all for likes, branches and follows; amount in DOGE for tips, 90/10). Without send: what it would send.";
    "dogefeed.registrar" Admin "fee_address?" "This arcade's D:// registrar (kid, key, address, fee address); fee_address sets where its 0.1 DOGE fees go.";
    "dogefeed.registrars_add" Admin "kid, pubkey, address, fee_address, name?" "Count another registrar's stamps (another arcade's) on this arcade.";
    "dogefeed.registrars_remove" Admin "kid" "Stop counting a registrar added with dogefeed.registrars_add.";
    "collections.known_set" Admin "on" "Ask Nintondo for its lists of Doginals, Pepinals and Bellinals (off by default): the listing once a day, a collection's pieces when somebody opens it.";
    "referee.get" Read "" "The referee arcade this one asks for seeds when a page names none, and whose key /r/verified trusts by default: {url, pubkey}, or {} when none is set.";
    "referee.set" Admin "url" "Name the referee arcade (https://...): its key is read from its /r/referee. An empty url clears it.";
    "realtime.lanes" Local "token" "Which players of the seat's room the fast lane reaches now (up), and how (routes: direct link or relay).";
    "realtime.status" Read "" "Rooms this arcade has players in, and the routes it knows to other arcades' rooms.";
    "ring.send" Local "page, to, line?, link?" "Ring an address from this arcade's player, for the app whose page asks (arcade.realtime.ring): rung, and why not. It reaches them only if they have Notifications on for that app and the caller in their address book.";
    "ring.waiting" Local "" "What is ringing this arcade's player now; asking also says the app is open, so it can be rung here.";
    "ring.get" Local "id, page" "The ring an app was opened from (arcade.realtime.ringed), for that app only: from, tag, link, line, at.";
    "inscribe" Wallet "data, encoding?, contenttype?, json?" "Inscribe content as this wallet's (arcade INSC; data as text or base64 with encoding: base64).";
    "inscribe.estimate" Public "data, encoding?, contenttype?, json?" "What inscribing that would cost: transactions, fee, value kept in its data outputs.";
    "wallet.requests" Read "page?" "Requests from pages to send, waiting for your answer.";
    "wallet.preview" Read "id" "A request's transaction as it would be built now: outputs and fee.";
    "wallet.approve" Local "id" "Approve a request: sign and send it.";
    "wallet.deny" Local "id" "Refuse a request.";
    "apps.owner" Read "page" "arcade.owner.identity: whether this wallet created the page and still holds it.";
    "apps.owner_send" Local "page, request" "arcade.owner.send: a page this wallet created and holds sends at once (testnet).";
    "apps.node" Local "op, page, to?, body?, after?, limit?" "arcade.node for a page: identity, send (sealed node-to-node), sent, replies.";
    "apps.launch" Read "chain, txid" "What an app window opens: the sandboxed page URL and the card.";
    // API keys (managed from this computer only)
    "apikey.list" Local "" "API keys (not their secrets).";
    "apikey.create" Local "name, perms, tip_limit?" "Make an API key; the secret is shown once.";
    "apikey.revoke" Local "id" "Revoke an API key.";
    "rpc.help" Public "" "Every command, its parameters and what it needs.";
    "rpc.info" Local "" "Where the RPC and the web API are, and the cookie file.";
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

pub fn help() -> serde_json::Value {
    serde_json::json!(COMMANDS.iter().map(|c| serde_json::json!({"method": c.name, "needs": c.perm.name(), "params": c.params, "summary": c.summary})).collect::<Vec<_>>())
}
