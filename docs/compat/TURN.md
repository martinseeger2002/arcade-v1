# TURN relay in the old arcade (mesh/turn.py, web/realtime.py) — what V1.0 must match

From the Arcade v1.0 session's read-only survey (2026-11-08), kept close to its text. Builds on UDP.md (the
STUN socket on --stun-port, default 3478, and the "st" proofs).

## 1. Implemented (turn.py)
- Methods (59-61, 292-327): Allocate 0x003, Refresh 0x004, CreatePermission 0x008, ChannelBind 0x009 (all
  authenticated); Send indication 0x0016 from the client (no auth, accepted only from an existing allocation's
  client address); Data indication 0x0017 to the client; ChannelData both ways. Responses: success
  0x100|method, error 0x110|method.
- Not implemented: FINGERPRINT, SOFTWARE in TURN responses, EVEN-PORT, RESERVATION-TOKEN, DONT-FRAGMENT, TCP
  allocations (RFC 6062), TLS/DTLS, IPv6 relay (RFC 6156).
- UDP only: REQUESTED-TRANSPORT must be 17, else 442 (381-383). Client side = the STUN port's dual-stack "::"
  socket (IPv4 or IPv6 clients). Relay side IPv4 only: XOR-PEER-ADDRESS must be family 1, 8 bytes, else
  rejected (105-110).
- Relayed port: no range; each allocation opens its own ephemeral IPv4 socket 0.0.0.0:0 with 4 MiB buffers
  (391-393). XOR-RELAYED-ADDRESS = that socket's outside address, learned by one STUN Binding to the first
  mesh-proved STUN server; no answer → (local LAN IPv4 or 127.0.0.1, local port) (403-414).
- Allocate success: XOR-RELAYED-ADDRESS, LIFETIME, XOR-MAPPED-ADDRESS, + MESSAGE-INTEGRITY (416-421).
- One allocation per client 5-tuple: a resend of the same tid gets the same answer; a different tid → 437 (374-380).
- Lifetime: default 600; requested clamped to max(600, min(3600, asked)). Refresh LIFETIME=0 deletes and
  answers 0 (436-447). Expired allocations swept on each authenticated request.
- Permissions: per peer IP, 300 s (PERMISSION_TTL) (454-463).
- ChannelBind (465-482): channels 0x4000-0x7FFF; CHANNEL-NUMBER a 4-byte attribute read as u16; one peer per
  number and one number per peer, conflict → 400; binding lasts 600 s (CHANNEL_TTL) and refreshes the IP's
  permission. The 600 s only limits peer→client ChannelData; client→peer ChannelData follows channels with no
  expiry. ChannelData to the client is padded to 4 bytes.
- Hairpin: a peer address that is another allocation on this server goes straight into it; the permission
  check allows it.
- Limits: MAX_ALLOCATIONS 256 total, MAX_PER_LOGIN 32 per username (486 over either). Token bucket per
  allocation 1.5 MB/s, total 25 MB/s; over → dropped silently. Monthly meter, default 100 GB: at the limit
  allocations get 486, traffic stops, the relay is no longer offered; saved to <home>/media-usage.json every
  60 s, resets on the UTC month. No per-IP limit.
- may_reach (118-127): never loopback, unspecified, multicast, link-local, or this node's own addresses.
  Global always allowed; private only when the client itself is private. Refused CreatePermission/ChannelBind
  → 403 or 400.

## 2. Auth: REST-style ephemeral long-term credentials (65-73, 329-368)
- username = f"{expiry_unix}:{who}", expiry = int(now + 21600) (LOGIN_TTL 6 h), who =
  blake2b(tag.encode(), digest_size=6).hexdigest() (12 hex). Tag: this node's players → the player's arcade
  address; a login borrowed over the mesh → f"{asking_peer_id}:{tag}" where the asker sends
  tag = f"{its node_id[:16]}:{player_address}".
- password = base64(HMAC-SHA256(secret, username.encode())), standard base64 with padding.
- secret = blake2b(bytes(mesh signing key), digest_size=32, person=b"arcm-turn"): per node, survives restarts,
  NOT shared across arcades.
- REALM "arcade". NONCE = f"{at}.{HMAC-SHA256(secret, f'{at}:{client_ip}').hexdigest()[:24]}": stateless, tied
  to the client IP, valid 600 s.
- 401 when MESSAGE-INTEGRITY or USERNAME missing, or username expired, or bad HMAC; 438 Stale Nonce for a
  bad/old nonce. Both challenges carry REALM and NONCE, no integrity.
- MESSAGE-INTEGRITY = HMAC-SHA1 with key MD5(f"{username}:arcade:{password}") (RFC 5389 long-term). No SHA256.
  Checking: header length rewritten to cover up to and including MESSAGE-INTEGRITY; attributes after it
  ignored. Responses signed the same way.
- Expiry checked only as int(username.split(":")[0]) > now; "who" only counts toward the 32-per-login limit.

## 3. Who may use it; mesh
- Logins only to signed-in players (/realtime/ice gives none to guests). Not tied to an app; anyone holding a
  valid login may use it until it expires.
- Borrowing logins for own players with "tu" frames (485-551): {"t":"tu","op":"ask","n":12-hex,"tag":≤64};
  answer {"op":"login","n","u":username,"p":password} or {"op":"no","n"} (no relay, unavailable, over limit).
  Fields capped at 128 chars. Borrower waits 2 s; the whole borrow inside /realtime/ice times out at 5 s.
- Only from directly linked peers with proved STUN addresses. The relay host:port always comes from the "st"
  "have" proof, never from "tu". Sorted by link RTT; keep 2; at least one IPv4 relay if any exists.
- No separate relay advertisement frame.

## 4. /realtime/ice (realtime.py:578-604)
- {"ok":true,"iceServers":[...],"relay":bool}, Cache-Control: no-store, no ttl.
- Order: a) up to 4 {"urls":"stun:h:p"} (or "stun:[v6]:p"; urls a STRING) from stun.known() (own proved first,
  then peers'); b) only if a) empty and realtime_public_stun not false: {"urls":"stun:stun.cloudflare.com:3478"};
  c) arcade relays, each {"urls":["turn:h:p?transport=udp", ...≤4],"username","credential"} (urls a LIST): own
  first if TURN available and an address is proved (asker private → (LAN IPv4, stun_port) first); if own IPv4
  proved, only own; if only IPv6 proved, add one borrowed IPv4 relay; if nothing proved or no TURN, the borrowed
  ones. Cached per player 120 s (60 s when empty). d) only if c) empty: operator's Cloudflare TURN (setting
  turn {key_id, token, apps[]}, cached 3 h, held back over turn.limit_gb).
- "relay" true when c) or d) gave anything.

## 5. Demux on the shared socket (stun.py:60-73,106-112; turn.py:292-327)
- STUN answers only an exact Binding Request (type 0x0001, magic 0x2112A442, top two bits zero,
  len == 20+length), with XOR-MAPPED-ADDRESS + SOFTWARE "arcade".
- Everything else → TURN: first byte 0x40-0x7F = ChannelData {u16 channel, u16 length, data} by client
  5-tuple; else parsed as STUN: Send indication → peer; the four requests authenticated; anything else dropped.
- Each allocation has its own relay socket: peer→client packets separated by socket; that socket's own mapping
  answer matched by tid. Peer packets → ChannelData if a live channel, else Data indication; sent back out the
  shared STUN socket.

## 6. Switches
--stun-port N (0 = no STUN and no TURN); ARCADE_MESH_TURN=0 (relay only off); setting media_server.limit_gb
(default 100); realtime_public_stun; turn {key_id, token, apps} (Cloudflare). status() → Admin {allocations,
month, used_gb, limit_gb, over}.
