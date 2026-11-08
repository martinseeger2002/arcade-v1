# UDP hole punching in the old arcade (Dogecoin Arcade beta) — what V1.0 must match

From the Arcade v1.0 session's read-only survey of arcade/mesh/ (2026-11-08), kept close to its text.
Files: stun.py (STUN Binding server), punch.py (punch helpers, NAT detection, admin "pu" probe), udp.py (UDP
transport; TCP→UDP move on an existing link, "ud" frames), introduce.py (links with no TCP, "ui" frames),
turn.py (TURN for browsers), relay.py (relayed links "rl"). Wiring service.py:963-1031. ARCADE_MESH_UDP=0 turns
off udp+introduce; ARCADE_MESH_TURN=0 turns off TURN.

## 1. Roles and frames (signaling in ordinary link frames over TCP/WS; only path packets over UDP)

### a) Public address: STUN reflection from other mesh nodes
- Each node runs RFC 5389 Binding-only STUN on UDP --stun-port (default 3478), dual-stack "::" V6ONLY=0,
  answers IPv4-mapped askers as IPv4 (stun.py:45-58,115-132). Same socket carries TURN (non-Binding → turn).
- STUN addresses proved over the mesh with "st" frames (stun.py:186-279):
  {"t":"st","op":"probe","n":hex12,"a":[[host,port]..≤4]} = "send a Binding request to my addresses";
  {"op":"probed","n","ok":[[h,p]..]} = those that answered; {"op":"have","s":[[h,p]..]} = my proved addresses,
  sent on link_up and when a new one is proved. Public addresses only. Rechecked every 6 h.
- The STUN list the UDP code uses = every peer's "have" offers. No public STUN for UdpShare (only the admin
  probe falls back to stun.l.google.com:19302 / stun.cloudflare.com:3478).

### b) NAT detection (punch.nat_kind, punch.py:384-392)
- Ask 2 STUN servers on different machines through the same socket; each request sent 3× within 1.5 s total.
- "open": every answer = local LAN IP; "unknown": <2 answers; "same-port": both answers same (host,port);
  else "new-port". ::ffff: answers normalized to IPv4. Mapping cached MAP_KEEP = 60 s.

### c) Introducers (no third-party rendezvous)
- On an existing link the two ends arrange the punch themselves over that link (udp.py).
- Two nodes with no link meet through "ui" frames routed hop by hop along node._via (the way the target's SUB
  came) (introduce.py:224-245): every node on the route introduces; forward only if hops < 8, ≤1 per sender
  per second, hops+1.
- A node with no links for 120 s publishes the introduction on chain: broadcast topic "mesh/intro/v1" body
  {"to": peer_id[:16], "s": base64 SealedBox(json header without "hops") to the peer's Ed25519 mesh key
  converted to X25519}; ≤ once per 1800 s, to 3 random announced keys (introduce.py:363-402).

### d) Frame fields
- "ud" (udp.py:27-29,739-814; link.py:182-205): try {n: 16-hex (8 random bytes), a: [[h,p]..≤4]};
  ready {n, a}; no {n, why}; home {n, a} (asker's LAN addresses, only after the answer proves both behind one
  NAT); go {} (last frame this side sends over TCP before switching to UDP); late {s: chunk seq, m: 0|1} +
  raw chunk blob; end {n: path.next_seq}. "ka" {} backup keepalive sent with every ping (PING_EVERY 15 s,
  first ping after 1 s).
- "ui" (introduce.py:22,205-211): {t:"ui", op:"hi"|"yes", to: peer node_id hex, from: own node_id hex,
  n: 16 hex, x: ephemeral X25519 public hex, a: offer, nat: nat_kind, ts: unix int, hops: 0, sig}.
  sig = Ed25519 by "from" over json.dumps({all fields except sig, hops, t}, separators=(",",":"),
  sort_keys=True) (introduce.py:65-81). Believed only if |now-ts| < 600 s. "yes" echoes the "hi" n.
- "pu" (punch.py:16-20, magic "APUNCH1") is admin-only diagnostics, not needed for compatibility.

### e) Address offers
- "a" = at most 4 IPv4 [host, port]. STUN-observed public addresses always; a LAN address only if the peer's
  offer contains one of our STUN-observed public IPs (same NAT), or mesh_lan_dial = "on" (punch.py:119-195;
  udp.py:625-643). Introductions never carry LAN addresses. Loopback only if STUN said loopback (tests).

## 2. UDP transport (udp.py)
- One UDP socket per node for all paths: IPv4 ephemeral, 0.0.0.0:0, 4 MiB buffers. NOT the TCP mesh port, NOT 3478.
- Packet: MAGIC "AUDP1" (5) | cid (8) | pn u64 BE | XChaCha20-Poly1305-IETF(plain, ad = those 21 header bytes,
  nonce = pn as 24 bytes BE, key_out) (udp.py:55-56,199-210).
- cid = blake2b(key_out, digest_size=8, person=b"arcm-udp-cid"); receiver routes by cid_in = cid(key_in).
- Replay window 1024 pns. Every send, resends included, uses a new pn.
- Plaintext starts with 1-byte kind:
  - 'H' hello: empty; answered with an ACK.
  - 'A' ack: u64 BE next wanted seq + 128-byte bitmap (1024 bits). Bitmap = int.from_bytes(big); bit i means
    seq rx_next+1+i, bit 0 = least significant bit of the big-endian integer. Wrong length → ignored.
  - 'D' data: u64 BE chunk seq | u8 more (1 = more chunks of this frame follow) | chunk ≤ 1120 bytes.
- A path counts as confirmed on the first ACK or DATA that decrypts.
- ARQ: cumulative ack + selective bitmap; ack after every 2 data packets or a 5 ms timer. RTO = clamp(srtt +
  4·rttvar, 0.2, 3.0), 1.0 before any sample, RFC 6298 smoothing; backoff rto·2^min(sends-1,4) capped at 1.5 s;
  fast resend when seq+3 ≤ highest selectively-acked seq and 1.5·srtt has passed. Window starts 16 chunks, max
  2048; +1 per ack below 256, else +16/window; halved (floor 16) at most once per RTT on loss. 24 sends of one
  chunk → path down. Queue limit 4096 chunks; receive limit 512 whole frames; a reassembled frame ≤ 64 KiB (+1 chunk).
- Timers: knock (hello to every target) every 0.2 s for 10 s; keepalive ACK if nothing sent for 1.5 s; dead
  after 6 s silence with data outstanding, or 20 s idle. Roaming: source address replaced if quiet > 2 s, or a
  home/LAN address answers while the current one is public.

### Keys and handshake
(i) UDP beside an existing TCP link (UdpShare): no new handshake. key_out = blake2b(link._send_key + nonce_bytes,
32, person=b"arcm-udp-path"); key_in same with _recv_key; nonce_bytes = bytes.fromhex(the "ud" n). Each path
frame = Link.pack(header, blob) in PLAINTEXT (u16 JSON length + JSON + blob, or binary FF FF 01 pub) — no link
seal, no u32 length prefix, TCP nonce counters untouched; the path AEAD replaces both.
(ii) UDP-only link (introducer): shared = X25519(eph, peer x); low = blake2b(shared+nonce, 32,
person=b"arcm-intro-low"); high = same with b"arcm-intro-high". The lower node_id (hex string compare) sends
with low. Then the ordinary handshake runs over udp.Stream with the same hello/AUTH bytes as TCP, initiator =
lower node_id, outbound = whoever said "hi". The Link writes normal TCP-wire bytes (u32 len + sealed) into the
path; the stream is a plain byte stream (one write+drain = one path frame). Wrong peer_id → drop ("wrong node").
No TCP backup: path dies → link dies.

### Hand-over and fallback (case i)
- Sender attach(): writes "go" over TCP, then sends everything over the path. Receiver holds UDP frames until
  it reads "go".
- On failure: sender writes every unacked/queued chunk as "late" over TCP, then "end" {n: next_seq}, then new
  frames over TCP. Receiver accepts late chunks (dedup by seq), waits up to 15 s until rx_next ≥ n and the
  queue drains, then drops the path. Each direction switches on its own.

## 3. Ports
Mesh UDP socket ephemeral. STUN/TURN on 3478 by default (--stun-port, 0 = none). Public address only via STUN
reflection from other nodes' proved STUN servers. No UPnP/NAT-PMP.

## 4. When it's tried
- UdpShare: on EVERY link, NAT or not. TRY_AFTER 3 s after link_up, again on each rebalance (~1 min). Only the
  lower node_id asks. Answerer refuses ("no", "not now") if the asker's id is higher, a path exists, or n isn't
  16 hex. Wait for "ready" 15 s. Retry after failure 60 s doubling to 3600 s, ×0.8–1.2 jitter; counter resets
  after a confirmed path later went down. Skipped when the link is already UDP-only. Fallback: stay on TCP.
- Introducer: a round every 20 s; while the node has links and is below its outbound target (or "everyone"
  mode under EVERYONE_MAX), meet up to 2 nodes whose SUB was heard within 120 s and that aren't linked or
  cooling. Cooling 300 s doubling to 6 h. Both send "hi" → the lower id's stands. Answerer refuses if inbound
  ≥ max_inbound. NAT strategy: both "new-port" → no attempt; peer "new-port" → spray 8 random ports
  (1024-65535) per knock at the peer's public IP for 22 s; we "new-port" → open 256 extra sockets each saying
  hello once a second, keep the one that gets through; else plain knock. Fallback: other mesh links, and
  relay.py relayed links ("rl", ≤ 3 hops) for Core/index sharing.

## 5. WebRTC
Separate (browser ICE). /realtime/ice returns mesh-proved node STUN servers, a public STUN only if none known
and allowed, then TURN logins from arcade relays (turn.py on the same 3478 socket; username carries expiry,
password = HMAC of it, valid 6 h). Shared with mesh UDP: only the STUN server and the "st" proofs.
