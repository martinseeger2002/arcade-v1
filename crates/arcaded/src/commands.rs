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
    "tx.coins" Public "addresses, chain?" "Spendable coins of up to 500 addresses (confirmed, not spent by a waiting transaction, and unconfirmed outputs), to build a transaction for a browser to sign. Home or installed chain, Addresses index on.";
    "tx.activity" Public "addresses, since?, chain?" "What changed for up to 500 addresses: history rows above height since, and waiting transactions paying or spending them; height to pass as since next time.";
    "tx.fee" Public "chain?" "The fee rate (per 1000 bytes) and dust limit this arcade builds transactions with.";
    "tx.broadcast" Post "hex, chain?" "Send a transaction signed elsewhere (a website user's browser): every input must spend a coin this arcade knows, outputs no more than inputs; pooled here and sent to the coin's network.";
    // Wallet
    "wallet.create" Local "phrase?" "Create a wallet (or restore one from 12 words). Returns the new words once.";
    "wallet.phrase" Local "" "The wallet's 12 words.";
    "wallet.keys" Read "chain?" "Your addresses on a chain, their @tags and balances.";
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
    "tag.claim" Wallet "tag" "Claim a @tag on the home chain.";
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
    "feed" Public "order?, channel?, tag?, author?, txid?, limit?, offset?" "Public posts (popular, new or friends) with replies, counts and tips.";
    "feed.channels" Public "" "Channels and how many posts each has.";
    "feed.settings" Read "set?" "Feed settings (hide muted, contacts only); set = {hide_muted, contacts_only} needs post.";
    "feed.post" Post "text?, channel?, file?" "Post publicly (needs a @tag).";
    "feed.act" Post "kind, target, text?, amount?" "Like, unlike, dislike, undislike, reply, share, edit, delete; tip (needs tip; amount in satoshis).";
    "feed.file" Public "txid" "A post's file (base64).";
    // Protocol indexes
    "ledger" Public "" "The arcade ledger's fingerprints next to what other arcades report.";
    "profile" Public "who" "A published profile (who = @tag or address).";
    "ord" Public "id?, chain?" "An inscription, or ord index counts.";
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
    "explorer.address" Public "chain?, address" "An address: balance, history, inscriptions, tokens.";
    "explorer.inscriptions" Public "chain?, offset?, limit?" "Inscriptions, newest first.";
    "explorer.inscription" Public "chain?, id" "One inscription.";
    "explorer.tokens" Public "chain?, offset?, limit?" "Tokens.";
    "explorer.token" Public "chain?, tick" "One token.";
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
    // Chains
    "chains.peers" Read "key" "A chain's P2P address book: nodes known, working in the last day, resting after failures; the working ones and the next to try.";
    "chains.list" Public "" "Every chain V1.0 knows, installed or not, with sizes reported by arcades.";
    "chains.status" Public "key" "One chain's staged progress.";
    "chains.offers" Public "" "Chains other arcades offer over the mesh.";
    "chains.sizes" Admin "" "Measure this arcade's disk use per chain now.";
    "chains.add" Admin "key, mode?, with?" "Install a chain (mode full, shared or chain; with = indexes).";
    "chains.pause" Admin "key" "Pause a chain.";
    "chains.resume" Admin "key" "Resume a chain.";
    "chains.index_add" Admin "key, index" "Add an index to a chain.";
    "chains.index_remove" Admin "key, index" "Remove an index from a chain.";
    "chains.remove" Admin "key" "Remove a chain and its data.";
    "chains.copy_status" Public "key?" "Core-data copies (cb) linked arcades offer for installed chains, the fetch and import in progress, and whether a chain's coin set is trusted from a copy or verified.";
    "chains.copy_start" Admin "key, what?" "Bootstrap a chain from the best copy offered: what = coins (the coin set; missing blocks are fetched from the network and stored, default) or all (the copy's blocks too).";
    "chains.copy_cancel" Admin "key" "Stop fetching a copy (its pieces are deleted) or decline the offer: the chain syncs the usual way.";
    "chains.copy_verify" Admin "key" "Re-check a coin set taken from a copy: derive it from the blocks again (ours replaces it if it differs).";
    // Borrowed coins
    "borrow.status" Read "" "Borrowed coins: the switches, and per chain its lenders, tip, floor, tip mismatch and your balance.";
    "borrow.set" Admin "on, chain?" "Turn borrowing on or off: for every coin (no chain) or for one chain (off removes its borrowed data).";
    "borrow.coins" Read "chain" "Your coins on a borrowed chain (from blocks read since the floor, and the lender's coin scan).";
    "borrow.send" Local "chain, to, amount, from?, send?" "Pay amount (satoshis) on a borrowed chain, signed here; send: true broadcasts to every lender, otherwise only built and shown.";
    // Mining
    "mine.start" Admin "threads?" "Mine the home chain.";
    "mine.stop" Admin "" "Stop mining.";
    "mine.status" Read "" "Mining state and hash rate.";
    // Apps (app cards)
    "apps.store" Public "sort?, q?, flagged?" "The App Store: app cards from every chain with the arcade ledger (popular or new; starred first).";
    "apps.installed" Read "" "Apps installed on this arcade, each at its newest version.";
    "apps.install" Admin "chain, txid" "Install an app (it follows its author's updates).";
    "apps.uninstall" Admin "id" "Uninstall an app.";
    "apps.star" Admin "chain, txid, on?" "Star or unstar an app (starred show first).";
    "apps.card" Public "chain, txid" "One app: card, versions, comments, permission switches.";
    "apps.perms" Admin "id, set?" "An app's permission switches (wallet, contacts, groups, media, pictures, notify).";
    "apps.review" Admin "chain, txid, verdict" "Clear or confirm an app flagged as a lookalike.";
    "apps.trust" Admin "address, on?" "Trust a publisher (never flagged as a lookalike).";
    "apps.storage" Local "page, op, key?, value?" "An app page's saves (load, set, remove, clear, history), as arcade.storage.";
    "realtime.join" Local "page, room, game?, member?, cred?" "Join a room over the mesh as this arcade's player (game defaults to the page): a token, me and the members. A website joins for its user with member (their address, or a guest id) and cred {a, x, s} signed over realtime.cert's words.";
    "realtime.cert" Public "address" "The words an address signs (Core signmessage) to play through this arcade, and their expiry: a website's user signs them in the browser and the site passes {a: address, x: expires, s: signature} to realtime.join.";
    "realtime.send" Local "token, data?, b64?" "Send to everyone in the room: text or base64 bytes, at most 512 bytes, about 5 a second.";
    "realtime.leave" Local "token" "Leave the room.";
    "realtime.poll" Local "token, wait?" "Room events for this seat (join, leave, message), waiting up to wait ms (15000 at most); a seat not polled for 30 s is let go.";
    "realtime.status" Read "" "Rooms this arcade has players in, and the routes it knows to other arcades' rooms.";
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
