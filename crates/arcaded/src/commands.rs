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
    "events" Read "n?, kind?" "Recent activity (newest first), optionally of one kind.";
    "mempool" Public "" "Home chain transactions waiting for a block.";
    // Wallet
    "wallet.create" Local "phrase?" "Create a wallet (or restore one from 12 words). Returns the new words once.";
    "wallet.phrase" Local "" "The wallet's 12 words.";
    "wallet.keys" Read "chain?" "Your addresses on a chain, their @tags and balances.";
    "wallet.link" Wallet "chain, wif, tag" "Link another wallet to your @tag (signed by both keys).";
    "wallet.unlink" Wallet "chain, address, tag" "Remove a linked wallet.";
    "wallet.links" Read "chain" "Your linked wallets on a chain.";
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
    "wallet.requests" Read "page?" "Requests from pages to send, waiting for your answer.";
    "wallet.preview" Read "id" "A request's transaction as it would be built now: outputs and fee.";
    "wallet.approve" Local "id" "Approve a request: sign and send it.";
    "wallet.deny" Local "id" "Refuse a request.";
    "apps.owner" Read "page" "arcade.owner.identity: whether this wallet created the page and still holds it.";
    "apps.owner_send" Local "page, request" "arcade.owner.send: a page this wallet created and holds sends at once (testnet).";
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
