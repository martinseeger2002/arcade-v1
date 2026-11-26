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
    /// Parameters: "name" required, "name?" optional, "a | b" one of them, "(...)" a note. Their types, defaults and
    /// allowed values come from [`PARAM_TYPES`] and [`PARAM_RULES`] ([`params_of`]).
    pub params: &'static str,
    pub summary: &'static str,
    /// The answer's shape, when the row gives one (`=> "{txid, fee}"`); [`result_of`] otherwise says what it can.
    pub result: Option<&'static str>,
}

macro_rules! cmds {
    ($($name:literal $perm:ident $params:literal $summary:literal $(=> $result:literal)?;)*) => {
        pub static COMMANDS: &[Cmd] = &[$(Cmd { name: $name, perm: Perm::$perm, params: $params, summary: $summary, result: cmds!(@r $($result)?) },)*];
    };
    (@r $r:literal) => { Some($r) };
    (@r) => { None };
}

/// The few commands kept off the network surfaces on purpose: answered on the unix socket alone, never by /api/v1
/// or JSON-RPC, and so not in [`COMMANDS`] (rpc.help, /api/v1/routes, openapi.json). Every other command is on every
/// surface with one name, one set of arguments, one answer shape and one set of error codes; a command's permission
/// (Local: the socket and the cookie RPC only) still decides who may call it. `surfaces_match` (tests) holds this.
pub const PRIVATE: &[(&str, &str)] = &[
    ("service.stop", "stops arcaded (arcade stop): the socket's own line, acted on by the stop watcher"),
];

cmds! {
    "status" Public "chain?" "Pepecoin testnet: sync state, tip, best_known (the best height anyone tells us of), tags, profiles, mesh links, node id, phase (starting, ready, stopping) and rules.update_needed. With chain (a key, e.g. arctest): that chain alone: tip {height, hash}, best_known (from linked peers, null when none told), best_known_from, behind, sync (synced, syncing, unknown, not_kept), mode and source (own or borrowed), and for the ARCADE chain its rules.";
    "peers" Read "" "Arcades linked over the mesh right now.";
    "mesh.udp" Read "" "UDP hole punching: STUN server and proved addresses, NAT kind, which links run over UDP paths (RTT, packets), introductions.";
    "mesh.udp_set" Admin "on?, lan_dial?, stun_port?, drop?" "Turn UDP paths and introductions on or off (ARCADE_MESH_UDP=0 overrides), offer LAN addresses to every peer, set the STUN port for the next start (0 = none); drop = a peer id prefix whose path to fail now (falls back to TCP).";
    "mesh.dials" Read "" "The last mesh dials (to a bare IP or a name, how long, why they failed), and a note when this network seems to refuse bare IPs while names connect (ARCADE #12).";
    "mesh.name" Read "" "The public name other arcades dial this one by, if set.";
    "mesh.sysinfo" Public "" "The machines behind the mesh, as their operators share them (opt-in, sysinfo.set): every other arcade's newest record heard in the last 24 h (CPU, cores, RAM, GPUs, disk, OS, version; seal speed in core-seconds per GiB, measured from its own seals), arcades sharing of arcades heard, network totals (cores, RAM, disk total and free, GPUs and VRAM where known, fastest, median and slowest seal speed), and this arcade's own. Reported, rounded, never on a chain.";
    "cpu.status" Read "" "How much of the computer this arcade takes: its CPU level (low, balanced, full = unthrottled), the cores background work may use of all this machine's, its priority (nice), the cap on the whole arcade in percent of the machine (none when unthrottled), the arcade's CPU use over the last two seconds, whether heavy work is holding back for the cap now, and the levels there are.";
    "cpu.set" Admin "level" "Set the CPU level: low (lowest priority, at most a quarter of the machine), balanced (low priority, at most half the machine; the default) or full, also called unthrottled (normal priority, every core, no cap: recommended only for machines dedicated to running an arcade). Kept across restarts; the cap applies at once, a lower priority too, a higher one may wait for the next start. Below full the cap covers all heavy work: script checks, block downloads and parsing, address builds, Core-copy imports, sealing, and the ARCADE chain's proofs of time, catalog and fraud watch.";
    "screening.status" Read "" "Content screening: the mode (borrow, own or off), whether a screener answers now and why not, the borrowed screening model in use (seller, prices, about what a text and a picture check cost, credit there, its last test) and every screening model on offer, the own model's address and last test, what was checked, and today's spending.";
    "screening.set" Admin "mode?, url?, model?, key?, pin?, unchecked?, day_cap?" "Choose how pictures and text from the chains are checked before this arcade's pages show them: borrow (the default: the cheapest screening model on the mesh that passes the built-in test, tested again every 6 hours and replaced when it drops), own (your own OpenAI-compatible model with vision at url, with model and key), or off. pin: borrow only \"model@<arcade id>\" (\"\" for the cheapest). unchecked: show_here (content not checked yet is shown on this computer, covered for others; the default) or cover (covered everywhere). day_cap: ARCADE base units a day for borrowed checks (1 ARCADE unless set).";
    "screening.test" Admin "" "Test the screening now: borrow looks at the offers again and tests the cheapest; own runs the built-in test on your model.";
    "screening.topup" Admin "amount" "Pay credit to the screening seller (the one in use, or the cheapest that needs credit), with the Models marketplace's memo, from this wallet. amount: ARCADE base units, at most 10 ARCADE. It counts once in a block.";
    "sysinfo.status" Read "" "Whether this arcade shares its system info, and the exact record it sends (or would send), field by field: what is reported, what is measured (seal speed, after 64 MiB sealed) and what is never shared.";
    "sysinfo.set" Admin "on" "Share this arcade's system info with the mesh (on: true) or stop (on: false; every arcade drops it at once). Off by default; kept across restarts and updates.";
    "mesh.name_set" Admin "name" "Set the public name other arcades may dial this arcade by (wss://host/mesh or host:port, a host name): shared with every link, so networks that reach only names can find it. \"\" clears it.";
    "mesh.relay" Read "" "Relayed links: whether this arcade relays links for others, its month's traffic limit and cap on links, what it has passed this month and today, the links it passes now, and this arcade's own links through relays.";
    "mesh.relay_set" Admin "on?, limit_gb?, max_links?, price_per_gb?, free_mb?, pay_to?, pay_on?, pay_cap?" "Relay mesh links between arcades that can't reach each other (end-to-end encrypted: the relay sees only sealed bytes). Off by default; limit_gb is the month's traffic limit (default 50), max_links the links passed at once (default 32); price_per_gb (ARCADE, default 0: free) and free_mb (a month per arcade, default 100) are the usage fee, paid to pay_to (any ARCADE address; default this arcade's wallet, and no price without one). pay_on (on unless set false) and pay_cap (ARCADE a month, default 1) let this arcade pay relays that charge, so it keeps its relayed links past a relay's free allowance.";
    "svc.attach" Local "service" "API socket only: attach a helper program for a service; the connection then carries its requests (docs/MODELS-APP.md).";
    "svc.offers" Read "service?" "Services offered over the mesh: each arcade's offer, its @tag, public name, pay_to, link state and out_of_reach (couldn't be linked lately).";
    "svc.services" Read "" "The service helpers attached to this arcade, with their offers and open requests.";
    "svc.ask" Local "to, service, body, timeout?, stream?" "Ask a service at an arcade (node id, or \"self\" for this one) over the mesh; answers collected (bytes as base64), or streamed line by line on the socket.";
    "svc.pay" Wallet "to, amount, memo, client_ref?" "Pay ARCADE to an address with a memo of up to 80 bytes (a service's credit).";
    "svc.paid" Read "txid, memo?" "Whether a payment is in a block, what it paid this arcade's address, and whether the memo is in it.";
    "events" Read "n?, kind?" "Recent activity (newest first), optionally of one kind. events.since follows the log with a cursor.";
    "mempool" Public "" "Pepecoin testnet transactions waiting for a block.";
    // For a website server in front of this arcade (its users hold their own keys)
    "id.start" Public "host, path?, http?" "ARCADE ID sign-in (no password; built on Digi-ID, created by the DigiByte community): a code for a website to show as a QR code, digiid://host/path?x=nonce (path default /digiid/callback; http: true for a plain-http test address). Digi-ID wallets answer it (Coinomi is the one digi-id.io names); an ARCADE wallet may answer with a key on any chain it holds. Lasts 5 minutes, works once.";
    "id.callback" Public "address, uri, signature, chain?" "What the wallet posted to the website's callback URL, checked: a code from id.start, not used or run out, signed by address (Core signmessage over uri: Digi-ID's DigiByte prefix, or with chain that chain's prefix or ARCADE's).";
    "id.poll" Public "nonce" "A sign-in code's state for the website's page: waiting, done (once: the address that signed and its chain; chain null = a Digi-ID wallet's key for this site), expired or unknown. The website links that address to its user.";
    "id.link_start" Public "key, host, via?" "Add a device with no password: the new browser's one-time public key (hex) in; a code, a 6-digit number and the QR text (arcadelink://host/code?k=key[&a=via]) out; via = this arcade's https address when the site has no node of its own. The user's other device scans it, shows the same number (the first 4 bytes of sha256(\"ARCADE/link\\0\" + code + \"\\0\" + key), mod 1,000,000) and sends the 12 words sealed to that key.";
    "id.link_send" Public "code, sealed" "The other device's sealed message for the new browser (sealed to its key: this arcade only passes it on), once per code.";
    "id.link_poll" Public "code" "The new browser collects what was sent: waiting, done (once: sealed and the key it is sealed to), expired or unknown.";
    "id.link_answer" Local "qr, send?" "The desktop app as the other device of a device link: an arcadelink:// text in. Without send: the site, the 6-digit number to compare with the new browser's and the @tag that would go. With send: the active account's 12 words sealed to the new browser's X25519 key (only it can open them) and sent to the arcade holding the link.";
    "id.box_put" Public "box, msg" "A sealed message for the other end of an ARCADE ID signing pairing (box: 32 hex characters only the two ends know; up to 16 waiting, 1,500,000 characters each, 64 MB in all boxes, ten minutes). This arcade only passes it on.";
    "id.box_take" Public "box" "Everything waiting in a box, oldest first; the box is emptied.";
    "id.sign_pair" Local "qr, send?" "Pair a website with this app so it can ask the app to sign (an arcadesign:// code in). Without send: the site, the 6-digit number to compare and the account, its Dogecoin address and its address on the chain its @tag is on, ARCADE (home). With send: paired; the site learns both keys (address, pubkey; home: {chain, address, pubkey}). The words never leave the app.";
    "id.sign_sites" Local "" "Websites paired with this app for signing.";
    "id.sign_forget" Local "id" "Unpair a website.";
    "id.sign_pending" Local "" "What paired websites are asking this app to sign: a message, a PSBT, or several PSBTs signed together, each with the coins it signs, every output, the fee and whether a signed coin carries an inscription. Signed: the account's own P2PKH coins and, only when named in inputToSign with ALL, an inscription reveal (a P2SH coin whose redeem script, carried in the PSBT, is a D:// style lock ending OP_CODESEPARATOR + the account's P2PKH; left as a partial signature).";
    "id.sign_decide" Local "id, ok" "Sign a waiting request (ok: true) or say no; the website gets the answer. The Dogecoin key signs unless the request names key \"home\" or the chain its @tag is on, ARCADE (chain): then the account's key there (messages with ARCADE's prefix; a PSBT's coins must be on that chain).";
    "id.answer" Local "code, chain?, send?" "Sign in to a Digi-ID site with this arcade's wallet (the active account), as a phone wallet such as Coinomi does: a digiid:// code in. Without send: the site's name, where the answer goes and this wallet's key for that site (SLIP-0013, never a key that holds coins; the same one the web wallet makes from the same words), to show first. With send: signed and sent to the site. chain: dgb (Digi-ID, the default) or another chain this wallet holds (ARCADE ID).";
    "id.relay" Public "uri, address, signature, chain?" "A website's user signed a Digi-ID code in the browser; the site's server passes the answer here and the arcade sends it to the code's site (a page may not post to another site; a server may). Only a well-formed code, signed by that address, once, and only to a public internet address (never this computer or its network; no redirects).";
    "vault.put" Public "tag, address, pubkey, blob, signature" "Password accounts: keep @tag's sealed record here (the 12 words sealed under the password in the browser: {kdf: PBKDF2-SHA512, iterations, salt, nonce, sealed}; nothing here can open it). address = the one @tag names on chain, or for a free @tag an address on the chain @tags are kept on, ARCADE (kept as a reservation, first come, until a claim on chain; a claim by anybody else ends it); signature = its Core signmessage (ARCADE's prefix) over \"ARCADE vault @tag address pubkey sha256\", sha256 of the record as {kdf, iterations, salt, nonce, sealed} JSON. Replaces an older one.";
    "vault.get" Public "tag, here?" "@tag's sealed record {tag, address, pubkey, blob, from?}: kept here, else (unless here) from the arcade that holds it. The shape of the original arcades' /signin/<tag>: a website serving it there lets other arcades find accounts made on it.";
    "vault.delete" Public "tag, signature" "Drop @tag's sealed record here (\"stop using a password\"): signature = @tag's address signing \"ARCADE vault delete @tag address sha256\" of the record kept. The account then opens on this site with ARCADE ID or its 12 words.";
    "tx.coins" Public "addresses, chain?" "Spendable coins of up to 500 addresses (confirmed, not spent by a waiting transaction, and unconfirmed outputs), to build a transaction for a browser to sign; mining rewards not mature yet listed apart (immature, spendable_after). Pepecoin testnet or an installed chain, Addresses index on.";
    "tx.activity" Public "addresses, since?, chain?" "What changed for up to 500 addresses: history rows above height since, and waiting transactions paying or spending them; height to pass as since next time.";
    "tx.fee" Public "chain?" "The fee rate (per 1000 bytes) and dust limit this arcade builds transactions with.";
    "swap.book" Public "" "Every open swap order of this arcade and the arcades linked to it (any leg for any leg, across chains).";
    "swap.place" Local "give, get, key?, payout?, maker_fee?" "Place a swap order (give a leg, get a leg). key {chain: secret hex} and payout {chain: address}: an order run with its own key for somebody else (a website visitor), its result paid to them; kept here, never shared. maker_fee {bps <= 500, to: {chain: address}}: the maker's operator's fee, paid in what the maker gives.";
    "swap.release" Local "order" "Give back an open order placed with its own key: cancelled, its key and payout addresses forgotten here.";
    "swap.quote" Admin "node, order, taker" "For a taker outside this arcade (a website visitor whose browser signs): ask the maker's arcade to hold its order for this taker (this arcade's name plus the visitor's reference) and give its terms, keys, lock time and whether its lock is sized.";
    "swap.told" Admin "node, order, hash, taker, taker_keys, taker_time, taker_height?, taker_lock" "For a taker outside this arcade: tell the maker's arcade the taker's lock is on the chain; it checks it on its own copy and locks its side.";
    "swap.letgo" Admin "node, order, taker" "For a taker outside this arcade whose lock step failed before anything was sent: let go of its hold on the order, so a retry or another taker needn't wait it out.";
    "swap.status" Admin "node, hash" "For a taker outside this arcade: the maker's side of a swap (its step and its lock).";
    "tx.broadcast" Post "hex, chain?" "Send a transaction signed elsewhere (a website user's browser): every input must spend a coin this arcade knows, outputs no more than inputs; pooled here and sent to the coin's network.";
    "psbt.send" Public "address, pubkey, chain?, to, amount" "Build, unsigned, a payment from a key that signs elsewhere (a website user's ARCADE app, through id.sign_pair): amount (smallest units) to an address or @tag of chain (default ARCADE when installed or borrowed here, else Pepecoin testnet), change back; coins as tx.coins lists them (never an inscribed one), the fee rate and dust as tx.fee. Answer {psbt (base64), chain, unit, address, what, summary: {inputs, outputs, fee}}. Nothing is signed or sent.";
    "psbt.feed_post" Public "address, pubkey, chain?, text, channel?, file?" "Build, unsigned, a public post from that key, the same record feed.post writes (and the original website builds): one transaction, a @tag needed. Answer as psbt.send.";
    "psbt.feed_act" Public "address, pubkey, chain?, kind, target, text?, amount?" "Build, unsigned, a feed act from that key, as feed.act writes it: like, unlike, dislike, undislike, reply, share, edit, delete, or tip (amount in the smallest units, paid to the post's author on that chain in the same transaction). Answer as psbt.send.";
    "psbt.finalize" Public "psbt, chain?" "A signed PSBT in (base64 or hex): every P2PKH signature checked and any partial one finished; the raw transaction {hex, txid, chain, size, fee} out, for tx.broadcast.";
    // Wallet
    "wallet.create" Local "phrase?" "Create a wallet (or restore one from 12 words). Returns the new words once.";
    "wallet.phrase" Local "" "The wallet's 12 words.";
    "wallet.keys" Read "chain?" "Your addresses on a chain, their @tags and balances (on SegWit chains each address type's address and balance under kinds; balance counts every type). balance follows the mempool (confirmed coins less those a waiting transaction spends, plus waiting outputs to you); confirmed is the blocks' figure; free the confirmed coins spendable now (not spent by a waiting transaction, mature, no inscription); pending / pending_coins what is arriving; waiting and dropped this wallet's transactions in the mempool and those that left it without a block.";
    "wallet.addr_kind" Wallet "chain, kind?" "The address type the Wallet shows on a chain (legacy, p2sh-p2wpkh, p2wpkh, p2tr; remembered per chain) and the types it offers; kind: remember that one.";
    "dd.wallet" Read "chain?" "Your DigiDollar on DigiByte (chain dgb, the default, or dgbtest): address (DD.../TD...), cents, coins, borrowed, whole (false when only coins from the borrow floor on can be seen).";
    "dd.coins" Read "chain?, pubkey" "The DigiDollar address and coins (txid, vout, cents, height) of any DigiByte key (33-byte hex), as dd.wallet shows this wallet's: for a website whose accounts sign in their browsers.";
    "dd.send" Local "chain?, to, cents, send?, client_ref?" "Send DigiDollars (cents, $1 to $100,000 per send) to a DD.../TD... address, with DGB from the same wallet for the fee (0.35 DGB/kB, at least 0.1, at most 0.5 DGB); send: true broadcasts, otherwise it's only built and shown.";
    "wallet.coins" Read "chain?, from?, kind?, all_kinds?" "Coin control: the coins a wallet.send with the same from, kind and all_kinds could spend (value, address, confirmations, age; status free, inscription or unchecked (small and nobody could check: kept back); frozen; spendable), with the chain's fee rule (fee_per_kb, per_started_kb, dust) and the change address (from).";
    "wallet.freeze" Wallet "chain, coins, frozen?" "Coin control: freeze coins (frozen true, the default) so no automatic send spends them and they can't be picked, or unfreeze them (false). The chain's frozen coins after.";
    "wallet.coin_prefs" Wallet "coin_control?, tip?" "Coin control's setting for the account in use (off until turned on) and whether its how-to tip shows; with either, change it first.";
    "wallet.send" Local "chain?, to, amount, from?, kind?, all_kinds?, send?, client_ref?, coins?" "client_ref (with send): as for ledger.item_open. coins (coin control): spend exactly these coins (\"txid:vout\", from wallet.coins), all of them and no other; refused when one is frozen, kept back for an inscription or spent, or they don't pay the amount and fee. Pay amount (satoshis) on a chain (Pepecoin testnet by default, or any installed or borrowed one) from this wallet (from: one of its addresses or an imported wallet's; otherwise the own address with the most), signed here; kind: the address type it's sent from, change back to it (default: from's type, else the one the Wallet shows); all_kinds: also spend the wallet's other address types' coins; send: true broadcasts, otherwise it's only built and shown with its fee (and a warning when the address could be meant for another chain).";
    "wallet.link" Wallet "chain, wif, tag" "Link another wallet to your @tag (signed by both keys).";
    "wallet.unlink" Wallet "chain, address, tag" "Remove a linked wallet.";
    "wallet.links" Read "chain" "Your linked wallets on a chain.";
    "account.signin" Local "tag, password, from?" "Sign in with an existing Dogecoin Arcade account (@tag + password): its words come from the arcade that holds it (from, or any we know), sealed under the password, then its imported wallets and mailbox. The account becomes the active one.";
    "account.add" Local "phrase" "Add a whole account from its 12 words (every chain's addresses, its @handle read from the chain) and use it. With no wallet here yet the words become this arcade's own wallet; words already here switch to that account.";
    "account.list" Read "" "Accounts on this arcade: its own wallet first, then accounts added or signed in; which one is active.";
    "account.switch" Local "id" "Use another account (\"\" = this arcade's own wallet): wallet, messages, groups and rooms follow it.";
    "account.signout" Local "id" "Sign an account out of this arcade: its words, keys, messages and groups here are forgotten.";
    "wallet.import" Local "chain, secret, addr_kind?, name?" "Import a wallet: a WIF key, or another wallet's words (one address, index 0); addr_kind legacy (default), p2sh-p2wpkh or p2wpkh. Kept encrypted here; nothing is published.";
    "wallet.imports" Local "" "Imported wallets, with balances on chains this arcade keeps addresses for.";
    "wallet.import_remove" Local "chain, address" "Forget an imported wallet and its key.";
    // Multisig vaults (crate::msigsvc; docs/MULTISIG.md)
    "multisig.key" Local "chain, wrap?" "This arcade's key for its next multisig vault on a (test) chain, to give the other co-signers: [fingerprint/48'/coin'/vault'/type']tpub.../<0;1>/*. wrap: sh (default where there is no SegWit), wsh (default on SegWit chains) or sh-wsh.";
    "multisig.create" Local "chain, cosigners, name?, wrap?, backup?" "Make a 2-of-3 vault of this arcade's next key and two other co-signers' keys (cosigners: a list, or one per line). Returns its descriptor: give it to each co-signer and keep it with your 12 words. backup: self (the mesh copy comes with mesh co-signing).";
    "multisig.import" Local "chain, descriptor, name?, backup?" "Add a vault made elsewhere from its descriptor (sh/wsh/sh(wsh(sortedmulti(2,...)))): watch-only, or signed for here when this wallet's phrase holds one of its keys.";
    "multisig.list" Local "chain?" "Your vaults (on one chain, or all) with their descriptors, co-signers (here or elsewhere) and balances.";
    "multisig.address" Local "id, index?, change?" "A vault's address: index on the receive (or change) branch; without index the next new receive address.";
    "multisig.coins" Local "id, lenders?" "A vault's unspent coins and balance, from this arcade's address index or borrowed chain data (there, lenders: false answers at once from the coins followed here; by default a lender's scan is added, which can take a minute).";
    "multisig.propose" Local "id, to, amount?, all?, coins?" "Propose a payment from a vault: amount (smallest units) to an address, or all: everything less the fee; coins (\"txid:vout\", a list) picks which coins, else the largest first. Change goes back to the vault's next change address. Returns the PSBT (give it to a co-signer) and its card; kept here as a proposal.";
    "multisig.card" Local "id, psbt?, proposal?" "What signing a vault spend approves: each output (payment or change back to the vault, checked by its key paths), the fee, each coin's signatures and whether this arcade sees it unspent, and whether it is ready.";
    "multisig.merge" Local "id, psbt" "Merge a co-signer's copy of a vault spend into the proposal kept here (a field the two disagree on is refused).";
    "multisig.sign" Local "id, psbt?, proposal?" "Sign a vault spend with this wallet's key in the vault (SIGHASH_ALL; refused when this arcade sees one of its coins spent). Returns the PSBT to pass on.";
    "multisig.finalize" Local "id, psbt?, proposal?, send?" "Finish a vault spend that has enough signatures (each checked) and, with send, broadcast it as tx.broadcast does; otherwise its hex.";
    "multisig.drop" Local "id, proposal" "Forget a vault spend proposal here.";
    "multisig.set_node" Local "id, place, arcade" "Name the arcade (its mesh node id, as multisig.key gives it) the co-signer key at place (from 0, as multisig.list shows the co-signers) is on, so spends can be sent there and taken from there; \"\" forgets it.";
    "multisig.share" Local "id, proposal" "Send a vault spend (as signed here so far) to every co-signer's arcade named on the vault, over the mesh (end-to-end encrypted). Each takes it only from an arcade named on its vault, and only for that vault's coins; nothing is signed there until its user signs.";
    "multisig.remove" Local "id" "Forget a vault here (its coins stay; its descriptor is needed to see or spend them again).";
    "tag.check" Public "tag, chain?" "Whether a @tag is free and valid on one chain (every chain has its own @tags): tag may name it (alice.doge), or chain does; else where new tags go. Answers full (alice.doge) and label (@alice · DOGE).";
    "tag.claim" Wallet "tag, chain?, client_ref?" "Claim a @tag on one chain: ARCADE, Pepecoin testnet (test coins asked for by themselves), or a mainnet chain tags live on here (pep, doge, dgb: kept with their ledger or borrowed; paid in that coin). tag may name the chain (alice.doge).";
    "tag.resolve" Public "tag" "What an @tag names: found one (its chain, address, label) or many (different people on several chains: each, to pick from) or none. alice.doge names Dogecoin's.";
    "start.status" Read "" "Getting started: wallet made, handle (claimed or being claimed), test coins asked for or here, synced.";
    "tag.list" Read "" "Your tag claims and their progress.";
    "faucet.ask" Wallet "index?, tag?" "Ask the mesh for test coins.";
    "faucet.arc" Wallet "address" "The user faucet: ask the mesh to top a website user's ARCADE address up to 0.05 ARCADE, in two coins (once per address a day; a website calls it when a user signs in). This arcade's own wallet gets 1 ARCADE once from the node faucet instead.";
    "ledger.write_chains" Read "" "Where this wallet can write its posts, acts and messages (ARCADE, Pepecoin testnet, and Pepecoin, Dogecoin, DigiByte where kept or borrowed): each with its balance, which is the default now, and whether ARCADE is stalled (writes then go to Pepecoin testnet). feed.post, feed.act, follow and msg.send take chain to pick one.";
    "ledger.lent" Read "" "Chains whose posts, acts, messages and profiles are also read from other arcades (their copy here borrowed or still catching up): rows held per table and how fresh.";
    "faucet.arc_status" Read "" "The ARCADE top-up asks this arcade knows and how many it paid today; node_faucet: node-gift asks, gifts paid today, nodes known paid, and this arcade's own gift.";
    "faucet.newcomers" Read "" "New arcades waiting for their welcome (1 ARCADE, so they can claim a @handle at once), this arcade's welcome settings, whether it can send one now, and the welcomers' high scores (who welcomed each new arcade first, read from the chain).";
    "faucet.welcome" Wallet "node, amount?" "Welcome a new arcade now from this wallet (\"Welcome to the arcade\"): this arcade's welcome amount, or amount (1 to 100 ARCADE, in satoshis). Refused once any arcade has welcomed it; the first payment on the chain gets the high-score point.";
    "faucet.welcome_set" Admin "auto?, amount?" "Welcome new arcades automatically (on unless turned off; after a 3-minute head start for people pressing the button) and the amount each gets (in satoshis: 1 to 100 ARCADE, 1 ARCADE unless set).";
    // Messaging
    "msg.me" Read "" "Your messaging key, fingerprint and whether it is published.";
    "msg.stats" Public "" "How many message candidates, posts, acts and machine records the ARCADE chain holds (Pepecoin testnet's when ARCADE can't be used here).";
    "msg.scan" Message "" "Look for new messages now (it also runs every 15 seconds).";
    "msg.who" Public "name" "Who a @tag or address is, and their published messaging key.";
    "msg.verify" Public "address, message, signature, prefix?, chain?" "Whether signature (Core signmessage, base64) is address's over message: with prefix, else chain's message prefix, else ARCADE's (\"Pepecoin Signed Message:\\n\"). {valid, address, prefix}.";
    "msg.conversations" Read "requests?" "Conversations, newest first (requests: people not in your contacts).";
    "msg.thread" Read "peer" "Every message with one person (their messaging key).";
    "msg.read" Message "peer" "Mark a conversation read.";
    "msg.unread" Read "" "Unread counts: messages, requests, group lines.";
    "msg.file" Read "id" "A message's file (base64).";
    "msg.send" Message "to, text?, file?, client_ref?" "Send a private message to a @tag, address or key; file = {name, type, data (base64)}.";
    "msg.delete" Message "id" "Stop showing a message here (the chain keeps it).";
    "msg.announce" Wallet "client_ref?" "Publish your messaging key now if it isn't on chain.";
    "profile.publish_addresses" Wallet "send?, client_ref?" "List this wallet's own addresses (Pepecoin mainnet, Dogecoin, each other coin installed here) on your @tag's profile, keeping the rest of it; no private key is pasted. send: true broadcasts, otherwise it's only built and shown with its fee and total cost.";
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
    "feed.post" Post "text?, channel?, file?, client_ref?" "Post publicly (needs a @tag).";
    "feed.act" Post "kind, target, text?, amount?, client_ref?" "Like, unlike, dislike, undislike, reply, share, edit, delete; tip (needs tip; amount in satoshis).";
    "follows" Public "who" "An account's followers and the accounts it follows (who = @tag or address), with @tags and counts.";
    "follow" Post "who, on?, client_ref?" "Follow an account (on: false unfollows): a feed act from this wallet (on ARCADE, or Pepecoin testnet while ARCADE is stalled); the newest between two accounts counts.";
    "feed.file" Public "txid" "A post's file (base64).";
    // Protocol indexes
    "ledger" Public "" "The arcade ledger's fingerprints next to what other arcades report.";
    "profile" Public "who" "A published profile (who = @tag or address).";
    "ord" Public "id?, chain?" "An inscription, or ord index counts.";
    "ord.send" Local "inscription, to, chain?, send?, client_ref?" "Send one inscription (id or number) from this wallet's address on a chain kept here with its ord index, or borrowed (where it sits from the arcades keeping the ord index, which must agree; coins from a lender, checked against their blocks; sent through the lenders): its coin whole to `to` first, clean coins pay the fee. Only one alone at the start of its coin. send: true broadcasts, otherwise it is built and shown with its fee.";
    "token.send" Local "propertyid, units, to, chain?, send?, client_ref?" "Send arcade tokens (base units) from this wallet in one transaction, on Pepecoin testnet's ledger or the ARCADE chain's (chain: arc / arctest), installed or borrowed; without send: true, only what it would do.";
    "token20.send" Local "chain, tick, amount, to, send?" "Send PRC-20 / DRC-20 tokens from this wallet (a job: inscribe the transfer, then send it), on a chain installed or borrowed here.";
    "token20.sends" Read "" "This wallet's PRC-20 / DRC-20 send jobs and where each stands.";
    "arc.cache" Admin "mb?, clear?" "The viewer cache of ARCADE blobs: its size limit (MB), what it holds, or clear it.";
    "ledger.announce" Wallet "client_ref?" "Publish this wallet's profile (messaging key under its @tag) on the ARCADE chain.";
    "ledger.profile" Read "address | tag" "A profile and @tag as the ARCADE chain's ledger holds them.";
    "ledger.send" Wallet "hex, client_ref?" "Send one arcade record (the whole payload after the marker) on the ARCADE chain. client_ref as for ledger.item_open.";
    "arc.keep" Admin "mb?" "Keep a share of the ARCADE chain's blobs by choice (rules version 5): whole segments, sealed and paid; segments short of copies first. mb: the share in MB (0 or \"off\": none).";
    "arc.fog" Read "" "Fog storage here: what it holds (own, kept, viewed), the segments it keeps, those short of copies on the network, warnings.";
    "arc.blob" Read "id" "A blob of the ARCADE chain (hex), fetched from linked arcades when it isn't here.";
    "arc.blob_publish" Wallet "path | hex, type, class?, gzip?, send?, client_ref?" "Put a file on the ARCADE chain as a blob. The fee is per started kB by class from rules version 5 (Temporary 0.000423, Release 0.00282, Permanent 0.003948, Boost 0.000282 ARCADE), 0.01 before. Copies kept: Permanent 3 from rules version 6 (7 before), Release 5, Temporary 3 for 90 days; a Temporary blob can be made Permanent later for the difference (arc.blob_upgrade). Without send, only the quote (blob_fee, fee).";
    "arc.blob_publish_large" Wallet "path | hex, type, class?, gzip?, name?, send?, client_ref?" "Put a file of up to 100 MB on the ARCADE chain as one large blob: parts of 4 MB over successive blocks, then a manifest naming them and the overall root; its id is the manifest's. Without send, the quote (parts, fee, blocks, seconds). Resumes after a restart without paying twice. arc.blob_publish switches to it above 4 MB.";
    "arc.blob_publish_status" Read "id?" "Publishing of large blobs from here: parts in blocks and waiting, the manifest, what was paid, resends.";
    "arc.blob_upgrade" Wallet "id, send?, client_ref?" "Make a Temporary blob Permanent (ARCADE rules version 6): pay the Permanent fee minus the Temporary fee for its bytes (0.003525 ARCADE per started kB), from this wallet; anyone may upgrade any Temporary blob, and it stays its publisher's. A large blob's manifest id upgrades all its parts, paid for their bytes too. Refused for an unknown, Permanent, Release or expired Temporary blob, and before version 6. Without send, only the quote." => "{blob, blobs, parts, bytes, already_paid, permanent_fee, difference, fee, at_height, floor, txid, hex, size, sent}";
    "a.profile" Read "tag? | address?" "A profile card on the ARCADE chain (yours when neither), with follower counts.";
    "a.profile_set" Wallet "name?, bio?, url?, picture?, client_ref?" "Publish your profile on the ARCADE chain (A://). Its addresses are only its wallet proofs, published by themselves: see a.card_addresses.";
    "a.card_addresses" Read "" "Every address of this wallet (each coin here, each imported wallet) and what your card shows for it: verified (proven on the chain), waiting (proof on its way) or removed.";
    "a.card_address" Wallet "coin, address, show" "Take one of your addresses off your card (show: false) or put it back (true); the new proof set is published at once.";
    "a.follow" Wallet "tag, client_ref?" "Follow an @tag on the ARCADE chain.";
    "a.unfollow" Wallet "tag, client_ref?" "Stop following an @tag.";
    "a.follows" Read "tag? | address?" "Who it follows and who follows it (yours when neither).";
    "a.contacts" Wallet "" "Your address book, opened with your key.";
    "a.contact_save" Wallet "id?, name, tag?, addresses, note?, client_ref?" "Save an address book entry (encrypted to your own key).";
    "a.search" Read "q" "Published profiles and @tags matching a name or @tag, for the address book.";
    "a.contact_add" Wallet "tag | address, client_ref?" "Add someone to your address book from their published profile.";
    "a.contact_delete" Wallet "id, client_ref?" "Remove an address book entry.";
    "a.wallets" Read "tag? | address?" "The wallet addresses someone has proven they hold on the ARCADE chain (every coin; A:// op 7), yours when neither; for yours also what would be proven now.";
    "a.wallets_publish" Wallet "force?, client_ref?" "Publish proofs of this wallet's addresses on every coin and of each imported wallet (each signs with its own key), when they differ from the chain's; done by itself after a profile is published.";
    "a.post" Wallet "text, client_ref?" "Post to your followers on the ARCADE chain.";
    "a.posts" Read "tag? | address? | following?, limit?" "Posts, newest first.";
    "a.reactions" Public "targets, me?" "Reacts, replies and tips on up to 100 targets (any chain's transactions: {chain, txid}) from the ARCADE chain's ledger (A:// ops 8 to 10, docs/APROTO.md): likes, dislikes, replies, tips (total, count, per recipient, author marked where known); me (true or an ARCADE address): its react and tips.";
    "a.replies" Public "chain, txid, offset?, limit?" "Replies to a target (any chain's transaction), oldest first, with their authors.";
    "a.react" Wallet "chain, txid, kind, client_ref?" "Like, unlike, dislike or undislike any chain's transaction, on the ARCADE chain (A:// op 8).";
    "a.reply" Wallet "chain, txid, text, client_ref?" "Reply to any chain's transaction (1 to 500 bytes), on the ARCADE chain (A:// op 9).";
    "a.tip" Wallet "chain, txid, amount, to?, memo?, client_ref?" "Tip on any chain's transaction: pay amount (sats of ARCADE) to `to` (an ARCADE address; default the transaction's author where one is found) with the tip record (A:// op 10, memo up to 100 bytes) in the same transaction.";
    "ledger.balances" Read "address" "An address's arcade token balances on the ARCADE chain (free, reserved, offered, accepted).";
    "ledger.tag_commit" Wallet "tag, client_ref?" "Commit ahead to claiming a released @tag on the ARCADE chain.";
    "ledger.tag_checkin" Wallet "client_ref?" "Keep this wallet's @tag on the ARCADE chain with a check-in (0.1 ARCADE into the pot).";
    "ledger.repo_push" Wallet "name, manifest, base?, force?, client_ref?" "Point @owner/name at a git manifest blob (owner or maintainer). From rules v4 the push names base (the manifest it was built on, \"\" for a new repo; default: the current one) and is refused if the repo moved on, unless force.";
    "ledger.repo_maintainer" Wallet "name, tag, add?, client_ref?" "Add or remove a maintainer of your repository.";
    "ledger.repo_fork" Wallet "from, name, client_ref?" "Fork @owner/name under your @tag.";
    "ledger.repo" Read "name" "A repository under an @tag: its manifest, maintainers and push history.";
    "ledger.repos" Read "owner?" "Repositories on the ARCADE chain.";
    "ledger.item_open" Wallet "repo, kind, title, body, from?, manifest?, head?, client_ref?" "Open an issue or a pull request on a repository. client_ref: a key of yours; sent again with the same key, the first answer comes back (replayed: true) and nothing is signed twice. The answer's receipt names what tx.status follows.";
    "ledger.item_comment" Wallet "repo, number, body, review?, client_ref?" "Comment on, or review (approve | changes), an issue or pull request. client_ref as for ledger.item_open.";
    "ledger.item_state" Wallet "repo, number, state, manifest?, client_ref?" "Close, reopen or merge an issue or pull request.";
    "ledger.items" Read "repo, number?, state?, kind?, since?, summary?, pending?, cursor?, limit?" "A repository's issues and pull requests with their comments and reviews, by number. Filters: state (open, closed, merged), kind (issue, pull), since (a height: items with anything at or after it, and each item's events trimmed to those at or after it); summary: true leaves out the events and keeps the title, plus last_by (who made the newest event). Every item carries last_height (its newest event's height, before since trims). Items, comments, reviews and state changes still waiting in the pool come too, marked pending: true (a pending item has no number yet; pending: false leaves them out). Paged: limit (default 50, at most 500) and cursor (the next of the page before; opaque); next is null on the last page, which also carries the pending items. count: how many on this page." => "{repo, items: [{number, kind, by, state, height, txid, text, events, comments, last_height, last_by?, pending}], count, next, limit}";
    "arc.earn" Admin "chain, on?" "The Earn ARCADE switch for a chain kept here in full (docs/EARN.md), and where it stands: sealing, the catalog, replicas waiting to register, this arcade's turn and next payout.";
    "arc.earning" Read "" "Whether this arcade earns ARCADE: a light (green, yellow, grey, red) and why, pay built up, audits, the next payout turn, and the account paid.";
    "ord.chunks" Public "from?, chain?" "Ord fingerprint chunks (as other arcades compare them).";
    "name" Public "name?, chain?" "A name claim, or counts.";
    "map" Public "map?, chain?" "A map claim, or counts.";
    "token" Public "tick?, chain?" "A token, or how many there are.";
    "tokens" Public "owner, chain?" "Token balances of an address.";
    // Explorer
    "explorer.summary" Public "chain?" "Chain summary for the explorer's front page.";
    "explorer.earn" Public "chain?" "This arcade's ARCADE pay, read only (docs/EARN.md): its turn, next payout, accrued pay, pay and draw weight, why pay waits, and its sealed replicas waiting to register; with a chain, that coin's Earn ARCADE progress too.";
    "explorer.blocks" Public "chain?, offset?, limit?, before?, n?, order?, from?" "Recent blocks, newest first. offset (back from the tip) and limit (at most 100) answer a page: {items, offset, limit, limit_applied (when the limit asked for was cut), next (the offset of the next page; null at block 0), chain, as_of, source}. before and n (at most 100) answer the plain list, as before. order asc with from (a height, default 0): a feed oldest first, {items, from, limit, next_from (the height to ask from next; the tip + 1 at the end: chains.wait waits for it), tip, chain, as_of, source}." => "{items: [{height, hash, time, txs}], next | next_from, chain, as_of, source}";
    "explorer.block" Public "chain?, id" "One block (id: a height, as a number or text, or a hash). Carries chain, as_of {height, hash} (the tip its confirmations count against), source (local, borrowed or mesh), unit and decimals.";
    "explorer.tx" Public "chain?, txid" "One transaction: height, block (its hash), confirmations, fee, inputs and outputs in base units (decimals: 8); an OP_RETURN output carries data (hex) and decoded (an arcade record: repository or A:// with its op). Carries chain, as_of, source; without chain it answers on Pepecoin testnet and says so (chain_note).";
    "explorer.address" Public "chain?, address, history?, inscriptions?, content?, history_offset?, history_limit?" "An address: balance, history (newest first, a page at a time: history_offset (or offset) and history_limit (or limit; default 50, at most 500); history_next is the next page's offset, null after the oldest; tx_count the total), inscriptions (the first 60; all with explorer.held), tokens. history: false or inscriptions: false leaves that part out; content: false lists inscriptions without previews. pending: the balance with the mempool taken in, what arrives and leaves, and the waiting transactions (when the mempool touches the address).";
    "explorer.richlist" Public "chain?, offset?, limit?" "A coin's rich list: the addresses holding the most of it, biggest first (rank, address, @tag, balance, coins, share of all coins held), the 500 biggest in pages of up to 100 (limit: 50 unless set); with the height and time it was made, how many addresses hold any, and building while one is being made. Made in the background from the Addresses index (hourly; the ARCADE chain every 10 minutes); asked of an arcade that keeps it for a chain kept here without addresses or only borrowed.";
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
    "explorer.a_reactions" Public "chain (the ARCADE chain), targets, me?" "The ledger's reacts, replies and tips on up to 100 targets ({chain, txid}; A:// ops 8 to 10, docs/APROTO.md), per recipient; what a.reactions reads, from any arcade keeping the ARCADE chain.";
    "explorer.a_replies" Public "chain (the ARCADE chain), target, offset?, limit?" "The ledger's replies to a target ({chain, txid}), oldest first; what a.replies reads.";
    "explorer.arc_ledger" Public "chain (the ARCADE chain), op, args" "An ARCADE ledger read (op: profile, a_profile, a_follows, a_posts, a_search, repo, repos, items, balances, contact_rows, cards, tag_rel, repo_row, holds_blob, wallet_state) on the answering arcade's own tables, about the address or tag in args; what arcades that only borrow ARCADE ask of arcades keeping it.";
    "explorer.arc_storage_summary" Public "" "ARCADE storage analytics: sealed segments by family, bytes, replicas and average copies, unheld segments, owners and genesis earners, total and drawing weight, the pot, issuance per month at the observed block time, blobs here by class, the viewer cache; fog storage (rules version 5): floors (each class's copy floor; boost: the copies a boost adds), short (segments and blobs below their floor: counts, each segment, the first 200 blob ids) and fog (rules on, grace and idle seconds, keep share, segments kept, blobs held and own here). From any arcade running ARCADE.";
    "explorer.arc_segments" Public "family?, source?, offset?, limit? (<= 500)" "Every confirmed (sealed) ARCADE segment, newest first: chain, family, range, bytes, root, poster, confirmation height, settled, replicas, weight each, and each holder (owner, node, tag, replica, audits missed in a row, draw-from height).";
    "explorer.arc_holders" Public "offset?, limit? (<= 500)" "Every ARCADE earner by pay weight: replica owners (replicas, bytes, pay and draw weight, share, audits passed/missed/running, accrued pay, serving verdicts, turn and next turn, punished, silenced) and genesis earners (address, keys, weight, accrued).";
    "explorer.arc_payouts" Public "blocks? (<= 1152, default 300)" "The last blocks' ARCADE coinbases by epoch: minted, coinbase total, the writer's share, what was released to each earner; with totals per earner and the pot now.";
    "fog.keep" Admin "segment | blob | publisher, mode?, preview?" "Keep data by choice (docs/STORAGEAPP.md): mode segment (default) makes this arcade a chosen keeper of the blob's whole segment (P#n / T#n; a publisher: every segment holding its blobs), counted once registered and audited, sealed and paid, inside the keep share (refused with the reason when the share is full, there is none, or the ARCADE chain is borrowed); mode pin keeps one blob (a large blob with its parts) as an uncounted extra copy that is never faded, uses the keep share and earns nothing. preview: true answers the card only (size, copies before and after, pay estimate, seal time on this machine) and changes nothing." => "{done, id, kind, card}";
    "fog.forget" Admin "segment | blob | publisher, preview?" "Forget data here: drops this arcade's copies (viewer cache, grace copy, pins) and never fetches or serves it again (mesh asks get not here, the holder bloom leaves it out); a blob this arcade published is un-pinned. A segment this arcade keeps is handed over first: it drops its copy only once a new keeper is registered and passed an audit (handing over n/floor in fog.mine), and stays a keeper while no arcade has room. Local only: nothing is deleted elsewhere or written on a chain. preview: true answers the card only." => "{done, id, kind, freed_bytes, handing_over, card}";
    "fog.undo" Admin "id, preview?" "Take back a fog.keep, pin or fog.forget (its id from fog.mine); a hand-over not finished stops and this arcade stays a keeper." => "{done, id, undone, card}";
    "fog.mine" Admin "" "What this arcade keeps by choice (segments, pins), forgot and hands over, with Undo ids; its keep share (none on a borrowed arcade); its blobs by role (keeper, publisher, pinned, grace, viewer) and how much of the map is in the fog for it. Operator only: the forget list is never public." => "{keep_share, chosen, pins, forgotten, handing_over, totals}";
    "explorer.arc_blobs" Public "id?, offset?, limit? (<= 500, default 200), publisher? (address or @tag), class? (permanent, release, temporary, boost), short_only?, here_only?, sort? (publisher, newest, copies)" "Every ARCADE blob as fog storage sees it (the Storage app's fog map), a large blob as one item with its parts: id, type, publisher (@tag, address), class, bytes, segment (P#n, T#n), holders by role (keeper, publisher, grace, viewer; registered+audited, registered, here or announced), copies counted against the floor (registered, audited replicas only), floor, short, announced keepers not counted yet, all copies, last viewed here, here, mine. With id: that blob and its journey (published, grace until, keepers' seal and audit heights, keepers that came and went, copies outside the floor). Totals for the filtered set: overall (copy bands), per publisher, per holder.";
    "explorer.arc_fog_nodes" Public "node? (id or prefix)" "Every node fog storage knows of from here (the fog map's node panel): keep share (setting, used, free), blobs held by role (keeper, publisher, grace, viewer), held and in the fog, segments kept (n/floor each), earning weight, and the 10 nodes it shares the most blobs with.";
    "explorer.arc_machines" Public "" "The machines behind the mesh for apps (the Storage app's Machines): the same as mesh.sysinfo. Join a record's node with an earner's key in explorer.arc_holders to show what it reports beside what it has proven.";
    "explorer.arc_nodes" Public "" "The Storage app's Nodes and Coins: every arcade on the mesh and this one (node, @tag, version, beta = an old arcade that can't seal, linked), each chain it keeps (mode, indexes, chain and index bytes: measured by it, else estimated from the network's figures), whether the chain is listed for ARCADE, its sealed bytes (the ARCADE registry's replicas, matched by node id) what is ready to seal now (blocks of a listed chain kept in full that are cut into segments, not sealed) and what is listed but not cut yet, its machine line when it shares system info; owners of sealed copies not on the mesh; per chain its keepers, size and how far it is cut and sealed; network totals. At most 500 arcades; the same answer for 10 s.";
    "explorer.msg_rows" Public "chain?, table (post, act, cand or profile), after?, limit?" "A page of one posts-and-messages table in key order after `after` (hex): {rows: [{k, v, t}], more, indexed}. What an arcade whose own copy is borrowed or catching up reads instead.";
    "explorer.msg_file" Public "chain?, txid" "The file a post carries, {txid, name, type, data (base64)}, at most 4 MB: what an arcade whose own copy is borrowed or catching up asks for a lent post's picture.";
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
    "chains.list" Public "" "Every chain V1.0 knows, installed or not, with sizes reported by arcades; per chain: hidden (chains.hide), queued (position), queued_resume and installing (the chain being installed now), and confirms {by, needs}: how confirmations count there (by \"stake\": a staked chain, blocks whose stake this arcade checked; \"work\": checked proof of work) and how many the other side's swap lock needs.";
    "chains.status" Public "key" "One chain's staged progress. `recovered_at_start` is `{from, to}` when this start wound the tip back (the last stop left without writing the coin caches, exit code 3), else null.";
    "chains.offers" Public "" "Chains other arcades offer over the mesh.";
    "chains.sizes" Admin "" "Measure this arcade's disk use per chain now.";
    "chains.add" Admin "key, mode?, with?, drive?, init?, core_dir?" "Install a chain (mode full, shared or chain; with = indexes; drive from chains.drives, else this arcade's own; init: headers agreed by several sources first, then every block stored only, then built in bulk, docs/INITSYNC.md; not given: init where the chain allows it, false: a plain install, true: init or a refusal; the answer says init_install and init_why_not; core_dir: the folder of a Core of this chain already on this computer, checked now and read in place when the install starts, instead of init). One install at a time: while another chain is still being installed it waits in a queue (queued, position).";
    "chains.queue" Public "" "Chains waiting in the queue, next first (position, key, name, mode, with; resume = a chain paused by chains.start_now that continues at its turn), the chain being installed now that they wait behind, the hidden chains, and plan_done (whether the startup chooser was answered).";
    "chains.unqueue" Admin "key" "Take a chain out of the install queue.";
    "chains.queue_first" Admin "key" "Move a queued chain to the front of the queue: it is next.";
    "chains.start_now" Admin "key" "Start a queued chain now: the chain being installed is paused and goes back into the queue right after it (a resume entry). Refused while Pepecoin testnet or the ARCADE chain is catching up.";
    "chains.hide" Admin "key, hidden?" "Leave a chain out of the Coins & chains list (hidden: true, the default) or show it again (false). Display only, any chain alike: an installed chain keeps running; nothing is removed or paused.";
    "chains.plan" Admin "keys?, mode?, skip?, core_dirs?" "The startup chooser: keys = the chains to use; core_dirs = {key: folder} for chains whose Core already runs on this computer (read in place at their turn instead of an init install; checked now). Every other chain is hidden; the chosen ones not installed or queued yet are queued, smallest reported size for mode (full, shared or chain; default full) first, unknown sizes last, with no indexes. The ARCADE chain (arctest) is installed when it is among keys and syncs first; left out, it stays borrowed from arcades that keep it. Never removes a chain. skip: true only marks the chooser as answered.";
    "chains.pause" Admin "key" "Pause a chain (by hand: the queue never resumes it).";
    "ui.link" Local "" "The link that opens the app's window in a browser (/ui/, with this start's key) and the SSH tunnel command for another computer (docs/WEBUI.md).";
    "chains.drives" Read "" "Drives a chain's file can go on: this arcade's own, and other mounted disks (free space, whether the arcade can write there).";
    "chains.move" Admin "key, drive" "Move an installed chain's file to another drive (drive from chains.drives; home = this arcade's own). Runs in the background; chains.status shows its progress.";
    "chains.resume" Admin "key" "Resume a chain (by hand: a resume entry for it leaves the queue).";
    "chains.index_add" Admin "key, index" "Add an index to a chain.";
    "chains.index_remove" Admin "key, index" "Remove an index from a chain.";
    "chains.remove" Admin "key" "Remove a chain and its data.";
    "chains.copy_status" Public "key?" "Core-data copies (cb) linked arcades offer for installed chains, the fetch and import in progress, and whether a chain's coin set is trusted from a copy or verified.";
    "chains.copy_start" Admin "key, what?" "Bootstrap a chain from the best copy offered: what = coins (the coin set; missing blocks are fetched from the network and stored, default) or all (the copy's blocks too).";
    "chains.copy_from_dir" Admin "key, dir" "Bootstrap a chain once from a Core datadir on this computer (dir: the folder holding blocks/ and chainstate/, e.g. an old arcade's Dogecoin Core), instead of a copy fetched over the mesh. Read in place and never written; refused while that Core runs (stop it first), if pruned, or of another chain. Taken in as an all copy (headers checked by proof of work, every coin against our blocks), trusted until re-checked from the blocks in the background. This arcade keeps its own database afterwards: the folder can be removed later.";
    "chains.copy_cancel" Admin "key" "Stop fetching a copy (its pieces are deleted) or decline the offer: the chain syncs the usual way.";
    "chains.refill" Admin "key, from?, to?, sources?" "Fetch back block bodies lost here (pruned, and no sealed copy answers for them) between from and to (default: the whole chain), 50 at a time: a batch is taken only when at least sources (default 2, at most 5) independent sources (different node ids and IP addresses: arcades holding the blocks, arcades lending their Core, coin P2P peers) sent the same bytes, each block matching our header, merkle root and stored txids. Runs in the background; see chains.refill_status.";
    "chains.refill_status" Public "key?" "Block refills (chains.refill): state, heights from/to/at, lost bodies found, restored, batches that couldn't be agreed, and the sources that agreed last.";
    "chains.refill_cancel" Admin "key" "Stop a block refill (what it restored stays).";
    "chains.check_rules" Admin "key, from?, to?, threads?, scripts?" "Check this arcade's stored blocks of a chain against the chain's consensus rules (the coin rules, and the scripts and block rules, where the chain has them), from..to (default the whole chain), the coins each block spends read from the blocks holding them (sealed ones included). Scripts in every block, or with scripts \"assume\" only above the newest checkpoint. Read only, in the background at the lowest priority on threads (default 2, at most 8); see chains.check_rules_status.";
    "chains.check_rules_status" Public "key?" "Rule checks (chains.check_rules): state, from/to/at and pct, blocks checked, transactions and inputs, blocks paying exactly the whole reward, blocks without a body here, and the first blocks that break the rules.";
    "chains.check_rules_cancel" Admin "key" "Stop a rule check.";
    "update.status" Public "" "Updates: this arcade's version, whether automatic updates are on (off on a fresh install, until the operator turns them on), the newest release, whether it is newer, when it last looked, what it is doing, and why it can't update itself if it can't (a local build, another system, a folder it can't write).";
    "update.set" Admin "on" "Turn automatic updates on or off. Off until turned on. When on, it looks for a newer release every 10 minutes and installs it only when the release is signed by a trusted release key, the archive hash matches that signed record, and the new program reports that version. An older version is never installed. Nothing skips the signature check. The old programs are kept as .prev, then a clean restart. Windows does this the same way.";
    "update.check" Admin "" "Look for a new release now (installs nothing).";
    "update.now" Admin "" "Install the newest release now, if it is newer than this one and it passes the signature, hash and version checks, and restart. An older version is refused.";
    "chains.rule_reports" Public "key?" "What arcades found checking each chain's rules not switched on while they connected its blocks (their first sync and every block since): this arcade's report and every one heard over the mesh, per arcade the heights checked, blocks paying the whole reward and blocks breaking them (scripts above the newest checkpoint); the broken blocks named; and the heights two or more arcades checked clean (clean_by_2). Evidence for switching a chain's rules on, not proof.";
    "chains.copy_verify" Admin "key" "Re-check a coin set taken from a copy: derive it from the blocks again (ours replaces it if it differs).";
    "chains.index_copy_status" Public "key?" "Index copies (docs/INDEXBOOT.md): what old arcades offer per chain, the fetch, its votes, and whether an index here came from a copy.";
    "chains.index_copy_start" Admin "key, index, from?" "Fetch an index copy now from the best offer (index = ledger or ord; from = one arcade's node id): it is taken in only if 2 or more other arcades agree with it.";
    "chains.index_copy_cancel" Admin "key, index" "Stop fetching an index copy (its pieces are deleted).";
    // V1.0 copies (docs/V1SHARE.md)
    "share.held" Public "" "V1.0 copies of chain parts held here (blocks, coins, ord, tokens, names, maps, ledger, profiles, messages): id, height, rows, size; and those being made.";
    "share.offers" Public "key" "V1.0 copies other arcades hold for a chain, per part: id, height, how many made it themselves and how many hold it.";
    "share.fetch" Admin "key, fam, id?" "Fetch a part's copy from every arcade holding it at once: the newest one 2 or more arcades made themselves (or the id given). Pieces are checked against the manifest; the copy is then held and served here too.";
    "share.install" Admin "key" "Install an added chain (or Pepecoin testnet) from V1.0 copies: asks the linked V1.0 arcades for blocks and coins copies a few blocks ahead, waits until 2 or more made them, fetched from every holder and taken in as they come (no replay); the coins are trusted until re-checked.";
    "share.install_cancel" Admin "key" "Stop installing a chain from copies.";
    "share.cancel" Admin "key, fam" "Stop fetching a copy (its pieces are deleted).";
    "share.serve" Admin "key, on" "Answer other V1.0 arcades' requests for copies of a chain (on by default): a copy is made when this arcade reaches the asked height, and deleted after 2 hours unused.";
    "share.make" Admin "key, fam" "Make a V1.0 copy of one part of an installed chain now, from one frozen view of the database (in the background; see share.held).";
    // Borrowed coins
    "borrow.status" Read "" "Borrowed coins: the switches, and per chain its lenders, tip, floor, tip mismatch and your balance.";
    "borrow.set" Admin "on, chain?" "Turn borrowing on or off: for every coin (no chain) or for one chain (off removes its borrowed data).";
    "borrow.coins" Read "chain" "Your coins on a borrowed chain (from blocks read since the floor, and the lender's coin scan).";
    "borrow.send" Local "chain, to, amount, from?, kind?, all_kinds?, send?, client_ref?" "Pay amount (satoshis) on a borrowed chain, signed here (kind, all_kinds: as wallet.send); send: true broadcasts to every lender, otherwise only built and shown.";
    // Mining
    "mine.start" Admin "threads?" "Mine Pepecoin testnet: only at the tip (synced with a coin peer, no higher height known); until then it waits and mine.status says why.";
    "mine.stop" Admin "" "Stop mining.";
    "mine.status" Read "" "Mining state and hash rate; waiting: why it is not mining now (not at the tip).";
    "stake.status" Read "" "KOT staking: on or off, coins, self-check, blocks staked; modifier_at (how far the stake pass has worked the chain out) and checks_on (staked blocks checked as they connect, and counted as confirmations).";
    "stake.set" Admin "on" "Turn KOT staking on or off.";
    "stake.check" Read "" "Check our KOT stake rules against the newest blocks.";
    // Once on the socket only (ARCADE #25 part 2: every surface answers the same commands; commands::PRIVATE lists
    // the few that stay off the network on purpose).
    "arc.block" Public "height? | id?, raw?" "One block of the ARCADE chain run here: by height (the branch it is on) or by id (any branch it holds): id, prev, height, time, difficulty, writer, score, steps, best, txs (txids), raw (with raw: true, the transactions' hex), bytes, pot_runs." => "{id, prev, height, time, difficulty, writer, score, steps, best, txs, raw, bytes, pot_runs}";
    "arc.submit" Post "hex, blobs?" "A signed ARCADE transaction (hex) into this arcade's pool, from where it goes to the other arcades and into a block; blobs: the hex of blobs it brings. As tx.broadcast for the ARCADE chain." => "{id, fee}";
    "arc.pool" Public "" "The ARCADE transactions waiting in this arcade's pool, in order of arrival (at most 1,000 listed)." => "{count, bytes, txs: [{id, fee, bytes}]}";
    "arc.catalog" Public "" "The segments the ARCADE chain lists, the entries that wait (and who stands behind each), and this arcade's side of it.";
    "arc.genesis" Public "" "Where the ARCADE chain's genesis list stands here.";
    "arc.segment_root" Read "chain, first, last" "The storage root this arcade makes for heights first..last of a chain it keeps.";
    "arc.replicas" Read "" "The sealed copies (replicas) this arcade holds, and a sealing under way.";
    "arc.sealing" Read "chain" "The segments of a chain as this arcade holds them for the ARCADE chain: waiting, queued, sealing, registering, earning; this arcade's weight and the chain's total; why sealing waits.";
    "arc.open" Read "segment, leaves" "Leaves of one of this arcade's replicas, opened (segment: its id; leaves: indexes).";
    "arc.seal" Admin "segment?" "Seal a confirmed segment (its id; else the first listed one of a chain kept here not sealed yet) under this arcade's key, in the background.";
    "arc.reveal" Admin "segment, first?, count?" "A reveal demand by hand: the holders of a listed segment must show these leaves on the ARCADE chain.";
    "arc.serve_check" Admin "owner, chain, first? | hashes?, count?" "A serving check by hand, no verdict posted: does the arcade `owner` (64 hex) hand out these blocks of chain?";
    "arc.keeper" Admin "on?" "This arcade's time keeper: on: true or false turns it on or off (it starts or stops within a block); its state.";
    "arc.archive" Admin "on?" "Keep every blob's plain copy (an archive node; on: true) or let the classes drop them.";
    "arc.blob_publish_many" Wallet "files, send?, client_ref?" "Several blobs in one ARCADE transaction: files [{hex | path, type, class?, gzip?}] (path: this computer's socket or cookie RPC only); without send, only what it would cost.";
    "code.status" Public "" "The releases known here and which of their files are held.";
    "code.add" Local "record, sig, files" "A release made here (scripts/code-publish.py), with its files ({name: path} on this computer).";
    "code.export" Local "id, dir" "A release's held files written into dir (a folder on this computer) under their names.";
    "swap.list" Read "" "This arcade's own swaps (as maker or taker) and the step each is at.";
    "swap.take" Local "node, order, give?, get?" "Take an order whole (node: the maker's arcade): the secret is made here and this wallet's leg locked; with the legs shown ({give, get}), only on exactly those terms.";
    "ledger.book" Public "network?, base?, quote?" "The market's open orders on the arcade ledger (docs/MARKET.md).";
    "market.holdings" Read "" "What this wallet can sell, on every chain it keeps, borrows or runs: each chain's coin, DRC-20 / PRC-20 where its tokens index is here, the ledger's tokens, and DigiDollars on an installed DigiByte chain (the Swap app's /r/market/holdings).";
    "ledger.trades" Public "network?, base?, quote?, before?" "The market's recent trades on the arcade ledger.";
    "ledger.pairs" Public "q?" "The market's pairs on the arcade ledger.";
    "explorer.site" Public "chain?, name" "A name's site as this arcade's names index holds it: {name, claim, holder, page, scanning}.";
    "explorer.sites" Public "chain?, all?" "Sites by name, newest first: {names: [{name, page, number, height, title}], scanning}.";
    "explorer.ledger_bytes" Public "chain?, id" "An arcade inscription's whole bytes (base64, up to 16 MB), for an arcade that does not keep this chain's ledger: {txid, number, content_type, sha256, b64}.";
    "explorer.page" Public "chain?, path, query?" "A public page read (GET /r/...) answered on this chain, for an app opened at an arcade that does not keep its ledger.";
    "explorer.app_cards" Public "chain?" "The rows of a chain that may be app cards, for an arcade's App Store that borrows it: {rows}.";
    "explorer.ord_outpoints" Public "chain?, outpoints" "Which of these outpoints carry ord inscriptions (outpoints: [\"txid:vout\", ...]).";
    "explorer.token_note" Public "chain?, id" "The token note an inscription carries (PRC-20 / DRC-20): {id, note}.";
    "explorer.prize_rows" Public "chain?" "The prize pool rows of a chain's ledger: {rows}.";
    "explorer.activity" Public "chain?, addresses, since?" "tx.activity read from this arcade's own copy (never borrowed): history rows and waiting transactions of up to 500 addresses.";
    // Agents never poll (ARCADE #25 part 2).
    "events.since" Read "cursor?, kinds?, limit?, wait?" "The arcade's event log, oldest first, after cursor (from events.since's next; none: from now, \"0\": from the oldest kept), of kinds (comma list or array) only; limit (default 100, at most 1,000); wait: seconds (at most 60) to wait for the first new event. {events: [{cursor, at, kind, text}], next, waited}." => "{events: [{cursor, at, kind, text}], next, waited, timed_out}";
    "chains.wait" Public "chain, height, timeout?" "Answered once chain's tip (as this arcade reads it: own copy or borrowed) reaches height, or after timeout seconds (default 60, at most 600): {chain, height, tip: {height, hash}, source, reached, timed_out}." => "{chain, height, tip: {height, hash}, source, reached, timed_out}";
    "rpc.openapi" Public "prefix?" "The OpenAPI 3.1 document of the web API (also GET /api/v1/openapi.json): every command, its typed parameters and result shape. prefix (the start of command names, e.g. tx. or ledger.item) keeps only those commands' paths; components stay whole." => "OpenAPI 3.1 document (object)";
    "rpc.guide" Public "" "The short guide for AI agents and scripts (also GET /api/v1/agents.md), as Markdown text." => "string (Markdown)";
    // Apps (app cards)
    "apps.store" Public "sort?, q?, flagged?, category?, withdrawn?" "The App Store: app cards from every chain with the arcade ledger (popular or new; starred first), in categories (counts in \"categories\"; category = one of them). Apps their publishers withdrew are left out; withdrawn: true lists only those.";
    "apps.categories" Public "" "The App Store's known categories (games, apps, social, node modules); a card may name any other.";
    "apps.installed" Read "" "Apps installed on this arcade, each at its newest version.";
    "apps.install" Admin "chain, txid" "Install an app (it follows its author's updates).";
    "apps.uninstall" Admin "id" "Uninstall an app.";
    "apps.star" Admin "chain, txid, on?" "Star or unstar an app (starred show first).";
    "apps.rows" Public "q?, offset?, limit?" "The apps for app pages (GET /r/apps, arcade.apps.list): every app the App Store lists, by name, public card data only, from the kept copy of the apps (made at most every 15 s). Each row {id, chain, chain_name, txid (its newest card), name, version, category, about, by (the publisher's @tag or null), creator, owner, icon (the cover's address on this arcade, /app/<chain>/<txid>, or null), trust, installed}. Cards carry no screenshots. q matches name, about, category and @tag; at most 500 rows a call." => "{apps, total, more}";
    "apps.open" Read "id, from? | page?" "What an app window needs to open another app (arcade.apps.open) in its own window, after the person says yes on the window's card: apps.launch's answer for the app with that id (one apps.rows lists). from: the asking app's id (or page: its page's txid); an app can't open itself." => "apps.launch's answer";
    "apps.card" Public "chain, txid" "One app: card, versions, comments, permission switches.";
    "apps.perms" Admin "id, set?" "An app's permission switches (wallet, contacts, groups, media, pictures, notify), its spending allowances: set {allowances: {<service>: ARCADE a day, or 0 to take it away}}, and its folder {granted, name} (never the path): set {folder: false} takes it away.";
    "apps.allowance" Read "id | page, service" "An app's spending allowance for a service: {per_day, spent_today, left, payments} in ARCADE, today (UTC).";
    "apps.allowance_use" Admin "id | page, service, amount, note?, undo?" "Count a payment (amount in ARCADE, not satoshis) an app asks for against today's allowance: {within: true} when it fits (counted at once; the app window then pays without a card), {within: false} when not (nothing counted). undo: true gives it back.";
    "apps.review" Admin "chain, txid, verdict" "Clear or confirm an app flagged as a lookalike.";
    "apps.trust" Admin "address, on?" "Trust a publisher (never flagged as a lookalike).";
    "apps.storage" Local "page, op, key?, value?" "An app page's saves (load, set, remove, clear, history), as arcade.storage.";
    "apps.folder_set" Admin "id, path" "Give an app one folder on this computer to read and write inside (arcade.folder): path is an existing folder inside the home folder (not the home folder itself, not hidden, not the arcade's own). The app window calls it after the person picks one. apps.perms shows {granted, name}; set {folder: false} takes it away." => "{granted, name}";
    "apps.folder_log" Admin "id, limit?" "What an app changed in its folder, newest first (at most 500): [{op (write, mkdir, remove), path, bytes, at}], with {granted, name}." => "{granted, name, changes}";
    "folder.browse" Local "path?" "The folder picker: the subfolders (names only, no files, no hidden ones) of path, or of the home folder; only inside the home folder (and other drives on Windows)." => "{path, name, parent, is_root, crumbs, dirs, more, roots}";
    "folder.list" Local "id, path?" "arcade.folder.list for a program: what is in a folder inside the app's granted folder (path relative to it)." => "{path, entries: [{name, kind, dir, size, modified}], more}";
    "folder.stat" Local "id, path" "arcade.folder.stat for a program: one entry in the app's folder, or {exists: false}.";
    "folder.read" Local "id, path, encoding?" "arcade.folder.read for a program: a file in the app's folder (at most 8 MB), as text or base64." => "{path, size, as, data}";
    "folder.write" Local "id, path, data, encoding?" "arcade.folder.write for a program: write a file in the app's folder (at most 8 MB; the folder at most 200 MB), logged." => "{path, size}";
    "folder.mkdir" Local "id, path" "arcade.folder.mkdir for a program: make a folder (and the ones above it) in the app's folder, logged." => "{path, made}";
    "folder.remove" Local "id, path" "arcade.folder.remove for a program: remove a file, or an empty folder, in the app's folder, logged." => "{path, removed}";
    "realtime.join" Local "page, room, game?, member?, cred?, fast?" "Join a room over the mesh as this arcade's player (game defaults to the page): a token, me and the members; fast: true also takes the fast lane (realtime.fast). A website joins for its user with member (their address, or a guest id) and cred {a, x, s} signed over realtime.cert's words.";
    "realtime.ice" Local "tag?, lan?" "ICE servers for one player (tag: their address or account, kept apart only for the relay's per-login limit): logins on this arcade's media relay where other arcades proved its STUN port open (lan: true adds its LAN address, for a player on its own network), else on relays borrowed from linked arcades; and this arcade's proved STUN. What a website hands its pages for WebRTC.";
    "realtime.cert" Public "address" "The words an address signs (Core signmessage) to play through this arcade, and their expiry: a website's user signs them in the browser and the site passes {a: address, x: expires, s: signature} to realtime.join.";
    "realtime.send" Local "token, data?, b64?" "Send to everyone in the room: text or base64 bytes, at most 512 bytes, about 5 a second.";
    "realtime.leave" Local "token" "Leave the room.";
    "realtime.poll" Local "token, wait?" "Room events for this seat (join, leave, message), waiting up to wait ms (15000 at most); a seat not polled for 30 s is let go.";
    "realtime.fast" Local "token, to, data?, b64?, reliable?" "Send to some players of the room over the fast lane between arcades (a seat joined with fast): text or base64 bytes, at most 16 KB, about 240 a second; to names the players (up to 64). Reaches those who play here or through an arcade with a lane up (realtime.lanes); unreliable sends waiting to go out are replaced by the newest, reliable ones keep their order. Heard as fast events.";
    "realtime.media" Local "token, to, b64" "A piece of an app window's (or a website viewer's) encoded camera, microphone or screen (up to 60 KB) for some players of the room: over the direct link to their arcade, else through at most two arcades on the room's route that have their media relay on; dropped when a link is behind. Heard as mchunk events (the window's or a website viewer's own, never the page's).";
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
    "dogefeed.post" Wallet "type?, text?, media?, reply?, quote?, pay?, ref?, tags?, mentions?, send?, client_ref?" "Post, comment, quote or edit on D:// from this arcade's own Dogecoin wallet: signed, stamped by this arcade and inscribed as DogeFeed writes them. Without send: the record and its cost.";
    "dogefeed.act" Wallet "op, target, amount?, send?, client_ref?" "Like, branch, follow, tip, reveal, delete or keyrevoke on D:// from this arcade's own Dogecoin wallet (1 DOGE in all for likes, branches and follows; amount in DOGE for tips, 90/10). Without send: what it would send.";
    "dogefeed.registrar" Admin "fee_address?" "This arcade's D:// registrar (kid, key, address, fee address); fee_address sets where its 0.1 DOGE fees go.";
    "dogefeed.registrars_add" Admin "kid, pubkey, address, fee_address, name?" "Count another registrar's stamps (another arcade's) on this arcade.";
    "dogefeed.registrars_remove" Admin "kid" "Stop counting a registrar added with dogefeed.registrars_add.";
    "collections.list_add" Admin "chain, name, file, slug?, description?, supply?, cover?" "Add (or replace) an operator list to a chain's collections index from a list file, nothing inscribed: OW.json ([{id, meta: {name: \"X #n\", attributes}}]), a doggy.market trait file, or [{id, number, traits}]. Shared on the mesh with its fingerprint (the sha256 an on-chain root of the same items carries).";
    "collections.list_remove" Admin "chain, slug" "Remove an operator list from this arcade.";
    "collections.list_copy" Admin "chain, slug" "Keep here a copy of an operator list another arcade shares, read from the mesh and kept only if it matches its fingerprint.";
    "collections.known_set" Admin "on" "Ask Nintondo for its lists of Doginals, Pepinals and Bellinals (off by default): the listing once a day, a collection's pieces when somebody opens it.";
    "referee.get" Read "" "The referee arcade this one asks for seeds when a page names none, and whose key /r/verified trusts by default: {url, pubkey}, or {} when none is set.";
    "referee.set" Admin "url" "Name the referee arcade (https://...): its key is read from its /r/referee. An empty url clears it.";
    "realtime.lanes" Local "token" "Which players of the seat's room the fast lane reaches now (up), and how (routes: direct link or relay).";
    "realtime.routes" Local "origin?" "For finding why a room doesn't connect: per arcade heard (origin: a node id or its start), the link its room sub came in on, the links up to it and the one room frames go out on, frames queued, and the rooms routed to it; and this arcade's own rooms with their ids.";
    "realtime.rtc" Local "token, peer, op, signal?" "A WebRTC link made by this arcade between an app window's seat (a room joined with fast) and a website viewer in its room, on the viewer's own wire (rtcsvc.rs): op offer (this seat's compact offer, when its id is the lower), signal (the viewer's compact offer or answer; an offer is answered), close. Once up (realtime.lanes: rtc), the window's realtime.media pieces and realtime.fast sends for that viewer go over it as RTP and data channels, and the viewer's come back as mchunk and fast events.";
    "realtime.rtc_status" Read "" "This arcade's WebRTC side: its UDP socket, candidates (host, STUN-mapped, relayed), relays and links.";
    "realtime.status" Read "" "Rooms this arcade has players in, and the routes it knows to other arcades' rooms.";
    "ring.send" Local "page, to, line?, link?" "Ring an address from this arcade's player, for the app whose page asks (arcade.realtime.ring): rung, and why not. It reaches them only if they have Notifications on for that app and the caller in their address book.";
    "ring.waiting" Local "" "What is ringing this arcade's player now; asking also says the app is open, so it can be rung here.";
    "ring.get" Local "id, page" "The ring an app was opened from (arcade.realtime.ringed), for that app only: from, tag, link, line, at.";
    "inscribe" Wallet "data, encoding?, contenttype?, json?, chain?, client_ref?" "Inscribe content as this wallet's (arcade INSC; data as text or base64 with encoding: base64). chain: where it is written (default the chain sends use now). A long inscription is sent in batches that stay inside the mempool limit of 25 unconfirmed transactions in a row, counting ones the wallet already has waiting; what was sent is kept and the rest goes out on its own as earlier ones confirm (inscribe.resume, or the same call again). A piece already broadcast is never signed again.";
    "inscribe.estimate" Public "data, encoding?, contenttype?, json?, chain?" "What inscribing that would cost on chain (default the chain sends use now): transactions, fee, value kept in its data outputs, and that chain's unit. Another chain is another quote.";
    "inscribe.status" Read "id?" "An inscription being sent from here (id from inscribe, or one of its txids), or every unfinished one: how many transactions are sent, how many are waiting in a row, and that the rest continues on its own.";
    "inscribe.resume" Wallet "id?" "Send more of an inscription that stopped because too many transactions were waiting in a row. With no id, every unfinished one. Pieces already broadcast are not signed again.";
    "wallet.requests" Read "page?" "Requests from pages to send, waiting for your answer.";
    "wallet.preview" Read "id" "A request's transaction as it would be built now: outputs and fee.";
    "wallet.approve" Local "id" "Approve a request: sign and send it.";
    "wallet.deny" Local "id" "Refuse a request.";
    "apps.owner" Read "page" "arcade.owner.identity: whether this wallet created the page and still holds it.";
    "apps.owner_send" Local "page, request" "arcade.owner.send: a page this wallet created and holds sends at once (testnet).";
    "apps.node" Local "op, page, to?, body?, after?, limit?" "arcade.node for a page: identity, send (sealed node-to-node), sent, replies.";
    "apps.launch" Read "chain, txid" "What an app window opens: the sandboxed page URL and the card; withdrawn {reason, replaced_by} when its publisher withdrew it (it still opens).";
    "apps.publish" Wallet "file | content_b64, type, name, card?, chain?, as?, send?, client_ref?" "Inscribe an app page, its icon or any file (at most 4 MB) signed with this arcade's own wallet key, exactly as arcade-publish writes it (arcade INSC, the card as its JSON): chain defaults to ARCADE when installed or borrowed here, else Pepecoin testnet; file is a path on this computer (not over the web API); as signs with the key holding that @tag. Without send: the plan (chunks, transactions, fee, locked, balance, enough). With send: {id (first txid, served at /app/<chain>/<id>), txids, chunks, fee}; on a coin chain the pieces wait for the split's first block (state waiting; sent in the background, or ask again). Asked again (same client_ref, or the same file, type, card and chain) it finishes what is unfinished and never pays twice." => "{chain, address, tag, name, content_type, bytes, chunks, transactions, state, id, txids, fee, resumed}";
    "apps.withdraw" Wallet "chain?, txid | family, reason?, replaced_by?, versions?, as?, send?, client_ref?" "Withdraw one of this wallet's apps from the App Store: a new version whose card says withdrawn (the whole app, or only the versions listed), published as apps.publish publishes. Only its publisher can: the app's creator must be the key signing here. replaced_by: chain:txid or a name. Without send: the plan and cost." => "apps.publish's answer + {withdrawal_card, app}";
    "apps.transfer" Wallet "chain?, txid | family, to, as?, send?, client_ref?" "Hand one of this wallet's apps to another owner (to: an address, or an @tag on the ARCADE chain): a new version whose card says transfer_to. From the block it is in, the new owner's cards of the same name are the app's next versions and nobody else's; the old owner can't publish it any more. The app keeps its id, so installs, permissions and allowances stay. Without send: the plan and its cost. A key-file publisher transfers the same way: arcade-publish --json '{\"app\": {\"name\": \"<name>\", \"family\": \"<family>\", \"transfer_to\": \"<address>\"}}' transfer.html.";
    // API keys (managed from this computer only)
    "apikey.list" Local "" "API keys (not their secrets).";
    "apikey.create" Local "name, perms, tip_limit?" "Make an API key; the secret is shown once.";
    "apikey.revoke" Local "id" "Revoke an API key.";
    "rpc.help" Public "method?, prefix?" "Every command, its parameters and what it needs. method (one exact command name) or prefix (the start of command names, e.g. tx. or ledger.item) answers only the matching ones.";
    "arc.status" Public "" "The ARCADE chain run here: height, tip, rules {now, known, protocol, update_needed, outdated_peers}, the pool, storage weights, owners, genesis earners and this arcade's pay (all from the chain). {on: false} when it doesn't run here.";
    "tx.status" Read "chain, txid | client_ref" "Where a transaction stands as this arcade can tell: state confirmed {height, block, confirmations}, accepted_local (in its pool; on the ARCADE chain broadcast {peers_linked, peers_sent, peers_holding, peers_acked, peers_refused}), broadcast_failed (refused here or by every peer that answered: broadcast.refusals [{peer, code, error}], code fee_too_low, conflict, missing_coin, dust, not_yet_valid, blob_missing, pool_full, invalid, rules or unreachable), signed (signed here: the receipt is written the moment it is signed, before any network step; in no pool or block yet), dropped (signed here, gone 30 min later) or unknown; ever_signed_here, error (what the send answered, when it failed after signing), and code not_found when this arcade never signed it." => "{chain, txid, state, ever_signed_here, signed_at, client_ref, cmd, confirmed?, broadcast?, error?, as_of?}";
    "tx.wait" Read "chain, txid | client_ref, until?, timeout?" "tx.status, answered once the transaction reaches until (accepted_local, broadcast_ok, seen_in_peer_mempool, confirmed; default confirmed) or is dropped or broadcast_failed, or after timeout seconds (default 60, at most 300: keep it under your client's socket timeout) with timed_out: true; reached (the milestone itself, not a failure first) and ended (until, broadcast_failed, dropped or timeout). Every tx.status answer carries milestones [{name, reached, by}] in order and milestone, the furthest reached." => "tx.status's answer + {timed_out, reached, ended}";
    "tx.resend" Wallet "chain, txid | client_ref" "Send again a transaction a send here signed (it has a receipt: tx.status ever_signed_here) and this arcade still holds the bytes of, the way it first went out: the ARCADE chain's own pool (offered to the linked arcades from there), a borrowed chain's lenders, or a kept chain's pool and peers. By client_ref, every transaction of that send, the first first; by txid, the ones the same send signed before it, then it. Nothing is signed again. Answers tx.status's shape for it (by client_ref, the send's last), with resent [{txid, ok, how (pool, lenders, peers, in_block) | error, code}]; one in a block already answers confirmed with resent []. Code not_found when nothing signed here matches; an error when it was signed here but its bytes are no longer kept." => "tx.status's answer + {resent}";
    "rpc.info" Local "" "Where the RPC and the web API are, and the cookie file.";
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}


// ---------------- typed parameters and answers (ARCADE #25 part 2) ----------------
//
// One source for rpc.help, /api/v1/routes and /api/v1/openapi.json: the rows above name each command's parameters;
// PARAM_TYPES gives every parameter name its JSON type and meaning; PARAM_RULES adds what one command alone says of
// one of its parameters (allowed values, a default, a limit). `every_param_is_typed` (tests) keeps the two in step.

/// Every parameter name a command takes: (name, JSON type, what it is). A type "a|b" takes either.
pub const PARAM_TYPES: &[(&str, &str, &str)] = &[
    ("activity", "boolean", "Also list the record's activity."),
    ("add", "boolean", "true adds, false removes."),
    ("addr_kind", "string", "Address type: legacy, p2sh-p2wpkh or p2wpkh."),
    ("address", "string", "An address on the chain in question."),
    ("addresses", "array|string", "Addresses (an array, or a comma list)."),
    ("after", "integer|string", "Start after this key or position (exclusive)."),
    ("all", "boolean", "All of them, not only the newest or own."),
    ("all_kinds", "boolean", "Also spend the wallet's other address types' coins."),
    ("amount", "integer|string", "An amount in the chain's smallest unit (satoshis; 8 decimals unless the answer says otherwise)."),
    ("arcade", "string", "An arcade's mesh node id (64 hex)."),
    ("args", "object", "The read's own arguments."),
    ("author", "string", "An author's @tag or address."),
    ("auto", "boolean", "Do it automatically (true) or only when asked (false)."),
    ("b64", "string", "Bytes as base64."),
    ("base", "string", "A base: a manifest blob id (64 hex) for a push, or a market's base asset."),
    ("before", "integer", "Only below this height or position (exclusive)."),
    ("bio", "string", "Profile text."),
    ("blob", "object|string", "A sealed record: {kdf, iterations, salt, nonce, sealed}; for fog.keep and fog.forget a blob id (64 hex)."),
    ("blobs", "array", "The hex of blobs a transaction brings."),
    ("blocks", "integer", "How many blocks back."),
    ("body", "string|object", "The body: text, or a JSON object where the command says."),
    ("box", "string", "A box id: 32 hex characters."),
    ("buyer", "string", "The other player's address."),
    ("category", "string", "An App Store category."),
    ("cents", "integer", "DigiDollar cents."),
    ("chain", "string", "A chain key (arctest, arc, peptest, pep, doge, dgb, ...). Absent: the command's default, Pepecoin testnet for most (the answer's chain says which)."),
    ("changes", "object", "The changes to make."),
    ("channel", "string", "A feed channel."),
    ("class", "integer", "Blob class: 0 permanent, 1 release, 2 temporary, 3 boost."),
    ("clear", "boolean", "Empty it."),
    ("client_ref", "string", "Your idempotency key for a send (1 to 128 printable characters): a retry with the same one never signs again; it answers the first send's result (replayed: true). tx.status and tx.wait take it in place of a txid."),
    ("code", "string", "A code from an earlier answer."),
    ("coin_control", "boolean", "Coin control on (pick which coins a send spends) or off (automatic)."),
    ("coins", "array", "Coins, each \"txid:vout\"."),
    ("frozen", "boolean", "true freezes (no automatic send spends them), false unfreezes."),
    ("tip", "boolean", "Show the how-to tip (true) or not (false)."),
    ("content", "boolean", "Include inscription content previews."),
    ("contenttype", "string", "A MIME type."),
    ("count", "integer", "How many."),
    ("counts", "boolean", "Include counts."),
    ("cover", "string", "A cover inscription id."),
    ("cred", "string", "A signed credential."),
    ("cursor", "string", "Opaque: the next from the page before. Absent: the first page."),
    ("data", "string", "Data: text, or base64 with encoding base64."),
    ("description", "string", "Free text."),
    ("dir", "string", "A folder on this computer."),
    ("direction", "string", "in, out or both."),
    ("dismiss", "boolean", "Hide the keep prompt for good (sets nothing)."),
    ("drive", "string", "A drive from chains.drives (home = this arcade's own)."),
    ("drop", "string", "A peer id prefix."),
    ("encoding", "string", "text or base64."),
    ("expect", "object", "The terms agreed: {give, take}."),
    ("fam", "string", "A chain part: blocks, coins, ord, tokens, names, maps, ledger, profiles, messages."),
    ("family", "string", "A segment family."),
    ("fast", "boolean", "Use the fast lane."),
    ("fee", "integer", "A fee in the smallest unit."),
    ("fee_address", "string", "Where fees go."),
    ("file", "object|string", "A file: {name, type, data (base64)}; for apps.publish, a path on this computer (the socket and cookie RPC only)."),
    ("content_b64", "string", "A file's bytes as base64."),
    ("card", "object|string", "An app card: the JSON beside the inscription, e.g. {\"app\": {name, family, version, ...}}; a string is used as written."),
    ("as", "string", "An @tag to sign with: the key here (the wallet's own, or an imported one) that holds it."),
    ("backup", "string", "Multisig vault descriptor backup: self (kept by you) or mesh (an encrypted copy for co-signers, not built yet)."),
    ("change", "boolean", "The change branch of a multisig vault (false: receive)."),
    ("reason", "string", "Why, in a few words."),
    ("replaced_by", "string", "What replaces it: chain:txid, or a name."),
    ("versions", "array", "Card txids (64 hex)."),
    ("withdrawn", "boolean", "Only the apps their publishers withdrew."),
    ("files", "array|object", "Files: [{hex | path, type, class?, gzip?}] for blobs; {name: path} for a release."),
    ("first", "integer", "The first height or leaf."),
    ("flagged", "boolean", "Only flagged ones."),
    ("following", "boolean", "Only accounts you follow."),
    ("force", "boolean", "Do it even when it would otherwise refuse."),
    ("free_mb", "integer", "Free MB a month per arcade."),
    ("from", "integer|string", "Where to start: a height, an address or a name, as the command says."),
    ("game", "string", "A game id."),
    ("gb", "number|string", "A size in GB (0 or \"off\": none)."),
    ("get", "object", "A leg to get: {chain, amount} or an asset."),
    ("give", "object", "A leg to give: {chain, amount} or an asset."),
    ("gzip", "boolean", "Store gzip-compressed."),
    ("hash", "string", "A hash (64 hex)."),
    ("hashes", "array", "Block hashes (64 hex each)."),
    ("head", "string", "A head manifest (64 hex)."),
    ("height", "integer", "A block height."),
    ("here", "boolean", "Only what this arcade holds (never ask other arcades)."),
    ("here_only", "boolean", "Only what this arcade holds."),
    ("hex", "string", "Bytes as hex."),
    ("hidden", "boolean", "Hide (true) or show (false)."),
    ("init", "boolean", "Not given: an init install where the chain allows it (headers agreed by several sources, every block stored only, then built in bulk; docs/INITSYNC.md). true: init or a refusal; false: a plain install."),
    ("core_dir", "string", "The data folder of this chain's Core already on this computer (holding blocks/ and chainstate/): read in place when the install starts, instead of init. Stop that Core first."),
    ("core_dirs", "object", "{chain key: Core data folder} for chosen chains whose Core already runs on this computer."),
    ("history", "boolean", "Include history rows."),
    ("history_limit", "integer", "History rows per page."),
    ("history_offset", "integer", "History rows to skip."),
    ("host", "string", "A host name."),
    ("http", "boolean", "Plain http (a test address)."),
    ("id", "integer|string", "An id: a height, hash, txid, inscription id or number, record id, as the command says."),
    ("ids", "array", "Ids (up to 500)."),
    ("include_invalid", "boolean", "Also list records that fail their checks."),
    ("index", "integer|string", "An index: a key index (faucet.ask) or an index name (ledger, ord, addresses, ...)."),
    ("inputs", "array", "The inputs a browser built."),
    ("inscription", "string", "An inscription id or number."),
    ("inscriptions", "boolean", "Include inscriptions held."),
    ("json", "object", "JSON content."),
    ("judge", "string", "A judge's address."),
    ("key", "string", "A key: a chain key (chains.*, share.*), a messaging key, or hex key material, as the command says."),
    ("keys", "array", "Chain keys."),
    ("kid", "string", "A registrar key id."),
    ("kind", "string", "A kind: what it is depends on the command (see its allowed values)."),
    ("kinds", "array|string", "Kinds to keep (an array, or a comma list)."),
    ("lan", "boolean", "Offer LAN candidates."),
    ("lan_dial", "boolean", "Offer LAN addresses to every peer."),
    ("last", "integer", "The last height."),
    ("leaves", "array", "Leaf indexes."),
    ("leg", "object", "A built trade leg."),
    ("limit", "integer", "Most rows in one answer."),
    ("limit_gb", "integer", "A month's traffic limit in GB."),
    ("line", "string", "A line of text."),
    ("link", "string", "A link."),
    ("lot", "integer", "A lot number."),
    ("maker_fee", "object", "{bps (<= 500), to: {chain: address}}."),
    ("manifest", "string", "A manifest blob id (64 hex)."),
    ("map", "string", "A map name."),
    ("max_links", "integer", "Most links at once."),
    ("mb", "integer|string", "Size in MB (0 or \"off\": none)."),
    ("me", "boolean", "Also say what this wallet did."),
    ("media", "array", "Media inscriptions."),
    ("member", "string", "A member id."),
    ("members", "array|string", "@tags or addresses."),
    ("memo", "string", "A memo (up to 80 bytes)."),
    ("mentions", "array", "@tags mentioned."),
    ("message", "string", "The signed message."),
    ("method", "string", "One exact command name (rpc.help)."),
    ("mode", "string", "How a chain is kept: full, shared or chain; for fog.keep: segment or pin."),
    ("level", "string", "A CPU level: low, balanced or full."),
    ("msg", "string", "A sealed message."),
    ("n", "integer", "How many."),
    ("name", "string", "A name."),
    ("network", "string", "A chain key for a market."),
    ("node", "string", "An arcade's node id (hex)."),
    ("nonce", "string", "A sign-in nonce."),
    ("note", "string", "A note."),
    ("coin", "string", "A coin's chain key."),
    ("coins", "array|string", "Coins as txid:vout (an array, or a comma list)."),
    ("cosigners", "array|string", "Other co-signers' keys ([fingerprint/path]tpub.../<0;1>/*): an array, or one per line."),
    ("show", "boolean", "Show it (true) or not (false)."),
    ("undo", "boolean", "Give back what was counted."),
    ("origin", "string", "An arcade's node id, or its first hex digits."),
    ("notes", "string", "Notes."),
    ("number", "integer", "An issue or pull request number."),
    ("offset", "integer", "Rows to skip (from the newest unless the command says)."),
    ("ok", "boolean", "Yes (true) or no (false)."),
    ("on", "boolean", "Switch on (true) or off (false)."),
    ("op", "string", "An operation: the command lists them."),
    ("open", "boolean", "Only open ones."),
    ("order", "string", "An order: a sort order (asc, desc, popular, new, ...) or an order id, as the command says."),
    ("outpoints", "array", "Outpoints \"txid:vout\"."),
    ("owner", "string", "An owner: @tag, address or node id."),
    ("page", "string", "An app page (chain:txid)."),
    ("password", "string", "The account's password."),
    ("path", "string", "A path: a file on this computer (socket and cookie RPC only), or a URL path, as the command says."),
    ("pay", "integer", "An amount to pay."),
    ("pay_cap", "number", "Most ARCADE paid a month."),
    ("pay_on", "boolean", "Pay relays that charge."),
    ("pay_to", "string", "An ARCADE address to be paid at."),
    ("payout", "object", "{chain: address}."),
    ("pct", "number", "A share of free space in percent (5, 10 or 25), recomputed as free space changes."),
    ("peer", "string", "A peer: messaging key or node id."),
    ("pending", "boolean", "Include what still waits in the pool (default true)."),
    ("perms", "array|string", "Permissions: read, message, post, tip, wallet, admin."),
    ("phrase", "string", "12 words."),
    ("picture", "string", "A picture inscription id or URL."),
    ("pool", "string", "A prize pool id."),
    ("prefix", "string", "A signed-message prefix; for rpc.help and rpc.openapi, the start of command names (e.g. tx.)."),
    ("price_per_gb", "number", "ARCADE per GB."),
    ("propertyid", "integer", "An arcade token's property id."),
    ("psbt", "string", "A PSBT (base64 or hex)."),
    ("pubkey", "string", "A public key (33-byte hex)."),
    ("publisher", "string", "A publisher's address or @tag."),
    ("preview", "boolean", "Only answer what it would do (the approval card); nothing changes."),
    ("q", "string", "Search text."),
    ("qr", "string", "The QR code's text."),
    ("query", "object", "Query fields."),
    ("quote", "string", "A quote: a market's quote asset, or a D:// record quoted."),
    ("raw", "boolean", "Include the raw transactions."),
    ("recipient", "string", "A recipient address."),
    ("record", "object|string", "A record."),
    ("ref", "string", "A D:// reference."),
    ("referee", "string", "A referee's address."),
    ("reliable", "boolean", "Resend until acknowledged."),
    ("replay", "object", "A run's replay."),
    ("reply", "string", "A D:// record replied to."),
    ("repo", "string", "A repository: @owner/name."),
    ("request", "object", "A send request."),
    ("requests", "boolean", "Only message requests."),
    ("review", "string", "A review verdict."),
    ("room", "string", "A room name."),
    ("scripts", "string", "\"assume\": skip script checks."),
    ("sealed", "string", "A sealed message."),
    ("secret", "string", "A WIF key or 12 words (local only)."),
    ("segment", "string", "A segment id (64 hex); for fog.keep and fog.forget a blob segment by name, P#n or T#n (as explorer.arc_blobs gives it)."),
    ("send", "boolean", "true signs and broadcasts; absent or false only builds and shows it (nothing signed)."),
    ("service", "string", "A service name."),
    ("session", "string", "A session id."),
    ("set", "object", "Settings to set."),
    ("short_only", "boolean", "Only what is short of its copy floor."),
    ("sig", "string", "A signature."),
    ("signal", "object", "A WebRTC signal."),
    ("signature", "string", "A Core signmessage signature (base64)."),
    ("since", "integer", "A height: only what is at or after it."),
    ("skip", "boolean", "Skip the chooser."),
    ("slug", "string", "A collection's slug."),
    ("sort", "string", "A sort order."),
    ("source", "string", "Where from: own, mesh, known, ..."),
    ("sources", "integer", "Most sources at once."),
    ("state", "string", "A state."),
    ("status", "string", "A status filter."),
    ("stream", "boolean", "Stream the answer line by line (socket)."),
    ("stun_port", "integer", "STUN port for the next start (0: none)."),
    ("summary", "boolean", "Short form: no events, the title only."),
    ("supply", "integer", "A collection's supply."),
    ("table", "string", "post, act, cand or profile."),
    ("tag", "string", "An @tag (the @ may be left out; alice.doge names Dogecoin's)."),
    ("tags", "array", "Tags."),
    ("take", "object", "What to take."),
    ("taker", "string", "The taker's reference."),
    ("taker_height", "integer", "The taker's lock height."),
    ("taker_keys", "object", "The taker's keys."),
    ("taker_lock", "string", "The taker's lock."),
    ("taker_time", "integer", "The taker's lock time."),
    ("target", "string|object", "A target: a txid, or {chain, txid}."),
    ("targets", "array", "Targets: [{chain, txid}] (up to 100)."),
    ("text", "string", "Text."),
    ("threads", "integer", "Threads to use."),
    ("tick", "string", "A token tick."),
    ("timeout", "integer", "Seconds to wait at most."),
    ("tip_limit", "integer", "A key's daily tip limit (smallest units)."),
    ("title", "string", "A title."),
    ("to", "string|array", "Where to: an address or @tag (svc.ask: a node id or \"self\"; realtime: player ids)."),
    ("token", "string", "A seat token from realtime.join."),
    ("txid", "string", "A transaction id (64 hex)."),
    ("type", "string", "A type: a MIME type for blobs, a record type for D://."),
    ("units", "integer|string", "Token base units."),
    ("until", "string", "The state to wait for."),
    ("uri", "string", "A digiid:// URI."),
    ("url", "string", "A URL."),
    ("model", "string", "A model's name, as its server lists it."),
    ("pin", "string", "One offer to use only: \"model@<arcade id>\"; \"\" for none."),
    ("unchecked", "string", "What happens to content not checked yet: show_here or cover."),
    ("day_cap", "integer", "A daily spending cap, in ARCADE base units."),
    ("descriptor", "string", "An output descriptor with its checksum (sh/wsh/sh(wsh(sortedmulti(...))))."),
    ("lenders", "boolean", "Also ask a lender of a borrowed chain (slower)."),
    ("place", "integer", "A place in a list, from 0 (a multisig vault's co-signer keys)."),
    ("proposal", "string", "A multisig vault spend proposal (its unsigned txid)."),
    ("value", "string", "A value."),
    ("verdict", "string", "A verdict."),
    ("via", "string", "This arcade's https address."),
    ("wait", "integer", "Milliseconds (realtime.poll) or seconds (events.since) to wait for news."),
    ("what", "string", "What to fetch: coins or blocks."),
    ("who", "string", "An @tag or address."),
    ("wif", "string", "A WIF private key."),
    ("with", "array", "Indexes to keep with a chain."),
    ("wrap", "string", "How a multisig vault is held: sh, wsh or sh-wsh."),
];

/// What one command says of one parameter: (command, parameter, allowed values (comma list, "" none), default (JSON,
/// "" none), most (a number's limit, 0 none)).
pub const PARAM_RULES: &[(&str, &str, &str, &str, u64)] = &[
    ("fog.keep", "mode", "segment,pin", "\"segment\"", 0),
    ("cpu.set", "level", "low,balanced,full,unthrottled", "", 0),
    ("screening.set", "mode", "borrow,own,off", "", 0),
    ("screening.set", "unchecked", "show_here,cover", "", 0),
    ("tx.wait", "until", "accepted_local,broadcast_ok,seen_in_peer_mempool,confirmed", "\"confirmed\"", 0),
    ("tx.wait", "timeout", "", "60", 300),
    ("chains.wait", "timeout", "", "60", 600),
    ("events.since", "limit", "", "100", 1000),
    ("events.since", "wait", "", "0", 60),
    ("events", "n", "", "30", 2000),
    ("ledger.items", "state", "open,closed,merged", "", 0),
    ("ledger.items", "kind", "issue,pull", "", 0),
    ("ledger.items", "limit", "", "50", 500),
    ("ledger.items", "pending", "", "true", 0),
    ("ledger.item_open", "kind", "issue,pull", "\"issue\"", 0),
    ("ledger.item_comment", "review", "approve,changes", "", 0),
    ("ledger.item_state", "state", "open,closed,merged", "", 0),
    ("explorer.blocks", "order", "desc,asc", "\"desc\"", 0),
    ("explorer.blocks", "limit", "", "20", 100),
    ("explorer.blocks", "n", "", "", 100),
    ("explorer.blocks", "from", "", "0", 0),
    ("feed", "order", "popular,new,friends,following", "\"popular\"", 0),
    ("feed.act", "kind", "like,unlike,dislike,undislike,reply,share,edit,delete,tip", "", 0),
    ("psbt.feed_act", "kind", "like,unlike,dislike,undislike,reply,share,edit,delete,tip", "", 0),
    ("a.react", "kind", "like,unlike,dislike,undislike", "", 0),
    ("wallet.send", "kind", "legacy,p2sh-p2wpkh,p2wpkh,p2tr", "", 0),
    ("borrow.send", "kind", "legacy,p2sh-p2wpkh,p2wpkh,p2tr", "", 0),
    ("wallet.addr_kind", "kind", "legacy,p2sh-p2wpkh,p2wpkh,p2tr", "", 0),
    ("wallet.coins", "kind", "legacy,p2sh-p2wpkh,p2wpkh,p2tr", "", 0),
    ("wallet.import", "addr_kind", "legacy,p2sh-p2wpkh,p2wpkh", "\"legacy\"", 0),
    ("multisig.key", "wrap", "sh,wsh,sh-wsh", "", 0),
    ("multisig.create", "wrap", "sh,wsh,sh-wsh", "", 0),
    ("multisig.create", "backup", "self,mesh", "\"self\"", 0),
    ("multisig.import", "backup", "self,mesh", "\"self\"", 0),
    ("chains.add", "mode", "full,shared,chain", "\"full\"", 0),
    ("chains.plan", "mode", "full,shared,chain", "\"full\"", 0),
    ("chains.copy_start", "what", "coins,blocks", "\"coins\"", 0),
    ("chains.index_copy_start", "index", "ledger,ord", "", 0),
    ("chains.index_copy_cancel", "index", "ledger,ord", "", 0),
    ("arc.blob_publish", "class", "0,1,2,3", "0", 0),
    ("arc.blob_publish_large", "class", "0,1,2,3", "0", 0),
    ("explorer.msg_rows", "table", "post,act,cand,profile", "", 0),
    ("explorer.dogefeed_address", "direction", "in,out,both", "", 0),
    ("apps.node", "op", "identity,send,sent,replies", "", 0),
    ("inscribe", "encoding", "text,base64", "\"text\"", 0),
    ("folder.read", "encoding", "text,base64", "\"text\"", 0),
    ("folder.write", "encoding", "text,base64", "\"text\"", 0),
    ("apps.folder_log", "limit", "", "500", 500),
    ("apps.rows", "limit", "", "100", 500),
    ("apps.rows", "offset", "", "0", 0),
    ("inscribe.estimate", "encoding", "text,base64", "\"text\"", 0),
    ("explorer.arc_segments", "limit", "", "", 500),
    ("explorer.arc_holders", "limit", "", "", 500),
    ("explorer.arc_payouts", "blocks", "", "300", 1152),
    ("explorer.token", "n", "", "25", 100),
];

/// A row's parameter list split at its top-level commas (a note in parentheses keeps its commas).
fn split_params(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for ch in s.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// A parameter's type and meaning from [`PARAM_TYPES`].
pub fn param_type(name: &str) -> Option<(&'static str, &'static str)> {
    PARAM_TYPES.iter().find(|p| p.0 == name).map(|p| (p.1, p.2))
}

/// One command's parameters, typed: [{name, type, required, default?, enum?, maximum?, one_of?, note?, description}].
/// "a | b": either one (one_of names the group; required when the group isn't marked optional).
pub fn params_of(c: &Cmd) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out = Vec::new();
    for part in split_params(c.params) {
        let (body, note) = match part.find('(') {
            Some(i) => (part[..i].trim().to_string(), Some(part[i + 1..].trim_end_matches(')').trim().to_string())),
            None => (part.clone(), None),
        };
        let alts: Vec<String> = body.split('|').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        let group_opt = alts.iter().all(|a| a.ends_with('?'));
        let one_of: Option<Vec<String>> = (alts.len() > 1).then(|| alts.iter().map(|a| a.trim_end_matches('?').to_string()).collect());
        for a in &alts {
            let name = a.trim_end_matches('?').to_string();
            let (ty, desc) = param_type(&name).unwrap_or(("string", ""));
            let mut v = json!({"name": name, "type": ty, "required": one_of.is_none() && !a.ends_with('?'), "description": desc});
            if let Some(g) = &one_of {
                v["one_of"] = json!(g);
                v["one_of_required"] = json!(!group_opt);
            }
            if let Some(n) = &note {
                v["note"] = json!(n);
                // "(<= 500)", "(<= 1152, default 300)"
                for bit in n.split(',').map(str::trim) {
                    if let Some(m) = bit.strip_prefix("<=").and_then(|x| x.trim().parse::<u64>().ok()) {
                        v["maximum"] = json!(m);
                    }
                    if let Some(d) = bit.strip_prefix("default").and_then(|x| x.trim().parse::<u64>().ok()) {
                        v["default"] = json!(d);
                    }
                }
            }
            if let Some(r) = PARAM_RULES.iter().find(|r| r.0 == c.name && r.1 == name) {
                if !r.2.is_empty() {
                    v["enum"] = json!(r.2.split(',').collect::<Vec<_>>());
                }
                if let Ok(d) = serde_json::from_str::<serde_json::Value>(r.3) {
                    v["default"] = d;
                }
                if r.4 > 0 {
                    v["maximum"] = json!(r.4);
                }
            }
            if name == "send" && v["default"].is_null() {
                v["default"] = json!(false);
            }
            out.push(v);
        }
    }
    out
}

/// What a command answers: the row's own shape, else what its kind of command answers.
pub fn result_of(c: &Cmd) -> serde_json::Value {
    use serde_json::json;
    if let Some(r) = c.result {
        return json!({"shape": r});
    }
    let p = c.params;
    let shape = if p.contains("client_ref?") {
        if p.contains("send?") {
            "{txid, fee, ..., receipt: {chain, txids, client_ref, state}} with send: true; without, the transaction built and shown (nothing signed)"
        } else {
            "{txid, fee, ..., receipt: {chain, txids, client_ref, state}}"
        }
    } else if c.name.starts_with("explorer.") {
        "object, or a list; reads of one chain carry chain, as_of {height, hash}, source and decimals"
    } else {
        "object (see summary)"
    };
    json!({"shape": shape})
}

/// Whether a command is a send that signs (it takes client_ref and leaves a receipt: crate::receipts).
pub fn is_send(c: &Cmd) -> bool {
    split_params(c.params).iter().any(|p| p.trim_end_matches('?') == "client_ref")
}

/// Whether a send only signs with send: true (it takes send).
pub fn signs_only_with_send(c: &Cmd) -> bool {
    split_params(c.params).iter().any(|p| p.trim_end_matches('?') == "send")
}

/// The web API route of a command.
pub fn route_of(name: &str) -> String {
    format!("/api/v1/{}", name.replace('.', "/"))
}

pub fn help() -> serde_json::Value {
    help_for(None, None)
}

/// Whether a command passes rpc.help's (and rpc.openapi's) filters: `method` names it exactly, `prefix` starts it.
/// Neither given: every command (2026-11-21, ARCADE #13: the whole list is about 300 KB, too much for an agent that
/// wants one command).
pub fn matches(c: &Cmd, method: Option<&str>, prefix: Option<&str>) -> bool {
    method.is_none_or(|m| c.name == m) && prefix.is_none_or(|p| c.name.starts_with(p))
}

/// rpc.help with its optional filters (see [`matches`]); no filter answers exactly what [`help`] always did.
pub fn help_for(method: Option<&str>, prefix: Option<&str>) -> serde_json::Value {
    serde_json::json!(COMMANDS
        .iter()
        .filter(|c| matches(c, method, prefix))
        .map(|c| serde_json::json!({"method": c.name, "route": route_of(c.name), "needs": c.perm.name(), "params": c.params,
                                    "summary": c.summary, "schema": params_of(c), "result": result_of(c), "sends": is_send(c)}))
        .collect::<Vec<_>>())
}

/// GET /api/v1/routes: every command as a route, with the same typed schema as rpc.help.
pub fn routes() -> serde_json::Value {
    serde_json::json!(COMMANDS
        .iter()
        .map(|c| serde_json::json!({"route": route_of(c.name), "method": c.name, "needs": c.perm.name(), "params": c.params,
                                    "summary": c.summary, "schema": params_of(c), "result": result_of(c), "sends": is_send(c)}))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names a match in `src` answers ("name" => or "a" | "b" =>), between the line holding `from` and the end.
    fn arms(src: &str, from: &str) -> std::collections::BTreeSet<String> {
        let body = &src[src.find(from).expect("the dispatch function")..];
        let mut out = std::collections::BTreeSet::new();
        for line in body.lines() {
            // The match's own arms (8 spaces in); a nested match (apps.node's ops) is further in.
            if line.starts_with("    }") {
                break;
            }
            let Some(t) = line.strip_prefix("        ").filter(|t| !t.starts_with(' ')) else { continue };
            let t = t.strip_prefix("c @ (").unwrap_or(t);
            if !t.starts_with('"') {
                continue;
            }
            let Some(head) = t.split(" =>").next().filter(|_| t.contains(" =>")) else { continue };
            let head = head.split(" if ").next().unwrap_or(head).trim_end_matches(')');
            let names: Vec<&str> = head.split('|').map(|x| x.trim().trim_matches('"')).collect();
            if names.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')) {
                out.extend(names.into_iter().map(String::from));
            }
        }
        out
    }

    /// Every command the socket answers is in the one table (so /api/v1 and JSON-RPC answer it under the same name,
    /// and rpc.help, routes and openapi.json list it), unless PRIVATE keeps it off the network; and every row has a
    /// handler. ARCADE #25 part 2: before, 32 commands answered on the socket alone.
    #[test]
    fn surfaces_match() {
        let api = include_str!("api.rs");
        let mut socket = arms(api, "fn dispatch_cmd(");
        // The explorer's own reads (crate::explorer::answer), reached on the socket by their prefix.
        let ex = include_str!("explorer.rs");
        // (and a read answered before the match: `if cmd == "explorer.arc_machines"`)
        socket.extend(ex.match_indices("cmd == \"explorer.").filter_map(|(i, _)| ex[i + 8..].split('"').next().map(String::from)));
        for line in ex.lines().filter(|l| l.contains(" =>") && l.trim_start().starts_with("\"explorer.")) {
            let head = line.split(" =>").next().unwrap_or("");
            socket.extend(head.split('|').map(|x| x.trim().trim_matches('"').to_string()).filter(|n| n.starts_with("explorer.") && !n.ends_with('_')));
        }
        let private: Vec<&str> = PRIVATE.iter().map(|p| p.0).collect();
        let missing: Vec<&String> = socket.iter().filter(|n| find(n).is_none() && !private.contains(&n.as_str())).collect();
        assert!(missing.is_empty(), "answered on the socket but not in the command table (so not on /api/v1, JSON-RPC, rpc.help, routes or openapi.json): {missing:?}");
        let unhandled: Vec<&str> = COMMANDS.iter().map(|c| c.name).filter(|n| !socket.contains(*n)).collect();
        assert!(unhandled.is_empty(), "in the table with no handler: {unhandled:?}");
        for (name, why) in PRIVATE {
            assert!(find(name).is_none() && !why.is_empty(), "{name} is private: it must stay out of the table");
            assert!(api.contains(&format!("\"{name}\"")), "{name} is listed private but the socket doesn't answer it");
        }
        // One name per command, and the web route maps back to it.
        let mut seen = std::collections::BTreeSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.name), "{} twice in the table", c.name);
            assert_eq!(route_of(c.name).strip_prefix("/api/v1/").unwrap().replace('/', "."), c.name);
        }
    }

    /// 2026-11-21 (ARCADE #13): rpc.help's method and prefix filters answer a strict subset holding what was asked.
    #[test]
    fn help_filters_by_method_or_prefix() {
        let all = help();
        let n = all.as_array().unwrap().len();
        assert_eq!(help_for(None, None), all, "no filter: the same answer as before");
        let names = |v: &serde_json::Value| v.as_array().unwrap().iter().map(|c| c["method"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let one = help_for(Some("ledger.items"), None);
        assert_eq!(names(&one), vec!["ledger.items"]);
        let tx = help_for(None, Some("tx."));
        let t = names(&tx);
        assert!(t.len() < n && t.contains(&"tx.wait".to_string()) && t.iter().all(|m| m.starts_with("tx.")));
        let li = names(&help_for(None, Some("ledger.item")));
        assert!(li.contains(&"ledger.items".to_string()) && li.contains(&"ledger.item_open".to_string()) && li.len() < n);
        assert!(help_for(Some("no.such"), None).as_array().unwrap().is_empty());
        let (a, b, c) = (all.to_string().len(), tx.to_string().len(), one.to_string().len());
        println!("rpc.help: {a} bytes; prefix tx.: {b}; method ledger.items: {c}");
    }

    #[test]
    fn every_param_is_typed() {
        for c in COMMANDS {
            for p in params_of(c) {
                let n = p["name"].as_str().unwrap();
                assert!(param_type(n).is_some(), "{}: parameter {n} has no type in PARAM_TYPES", c.name);
            }
        }
        for r in PARAM_RULES {
            let c = find(r.0).unwrap_or_else(|| panic!("PARAM_RULES names {}, no command", r.0));
            assert!(params_of(c).iter().any(|p| p["name"] == r.1), "PARAM_RULES: {} takes no {}", r.0, r.1);
        }
        let t = |c: &str, p: &str| params_of(find(c).unwrap()).into_iter().find(|x| x["name"] == p).unwrap();
        assert_eq!(t("tx.wait", "until")["enum"][0], "accepted_local");
        assert_eq!(t("tx.wait", "timeout")["maximum"], 300);
        assert_eq!(t("tx.status", "txid")["one_of"], serde_json::json!(["txid", "client_ref"]));
        assert_eq!(t("tx.status", "chain")["required"], true);
        assert_eq!(t("explorer.arc_payouts", "blocks")["default"], 300);
        assert_eq!(t("wallet.send", "send")["default"], false);
        assert_eq!(t("ledger.items", "cursor")["type"], "string");
        assert!(is_send(find("ledger.item_open").unwrap()) && !is_send(find("ledger.items").unwrap()));
        assert!(signs_only_with_send(find("wallet.send").unwrap()) && !signs_only_with_send(find("ledger.item_open").unwrap()));
        let h = help();
        assert_eq!(h.as_array().unwrap().len(), COMMANDS.len());
        assert!(h[0]["schema"].is_array() && h[0]["result"]["shape"].is_string());
    }
}
