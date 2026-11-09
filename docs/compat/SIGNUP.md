# Accounts in the old arcade: sign-up, vaults, sign-in — what V1.0 must match

From the Arcade v1.0 session's read-only survey (2026-11-08). See also accountsvc.rs (V1.0 sign-in, built).

## Vault (sealed words)
{"kdf":"PBKDF2-SHA512","iterations":600000,"salt":hex16,"nonce":hex12,"sealed":hex}
- sealed = AES-256-GCM (ciphertext||tag) of UTF-8 of the normalized phrase (lowercase words, single spaces).
- key = PBKDF2-HMAC-SHA512(password UTF-8, salt, iterations) → 32 bytes. No AAD. Readers use blob.iterations.
- Server caps stored text at 20000 chars.
- MIN_PASSWORD = 12 code points; leaked-password refusal via HIBP k-anonymity (SHA-1, first 5 hex to
  https://api.pwnedpasswords.com/range/<5hex> with header Add-Padding: true; refuse when count > 0; if the list
  can't be reached, let it through; ARCADE_LEAKED_URL "" turns it off). Both are client-side only.

## POST /signup (web/app.py:12928-13066)
Body: {"tag", "pubkey": login Ed25519 public key hex (m/24946'/1'/0' of the words), "address": tag-chain
address (m/44'/1'/0'/0/0 on Pepecoin testnet), "coin_pubkey": hex (that address's pubkey),
"address_<net>", "coin_pubkey_<net>" for each other chain the arcade's /account chains list names,
"blob": {vault}}
- Other-chain entries blank or invalid are skipped silently.
- Checks in order: tag valid (2-24 a-z 0-9 _, lowercased, @ stripped, not reserved) else 400; blob dict with
  truthy "sealed" else 400 "that is not an encrypted wallet"; address valid on the tag chain; if the token
  index says ANOTHER address holds the tag → 400 "@x is claimed on the chain by another address" (a claim is
  NOT required first); if not on chain and not here → _vault_elsewhere(tag, 4 s): another arcade holding it
  under a DIFFERENT pubkey → 400 "@x already has an account on <host>: sign in instead…" (same pubkey OK);
  pubkey already registered here → 400 "this key already has an account on this arcade: sign in instead";
  seats full → 409; name taken here → 400.
- No proof of key ownership, no captcha, no sign-up rate limit (GET /signup/{tag} availability: 20/300 s/IP).
- Then: stores keys, watches addresses, faucet pour (test chains GIFT = 100 coins once per pubkey; mainnet
  nothing unless enabled, ≤10).
- 200 {"tag","pubkey","address","fruit":bool,"given":<sat>,"no_coins":"<reason or ''>"} and sets a session
  cookie. Errors {"detail": "..."} 400/409.

## New handle end to end (signup.html; wallet_js.js:329-367, 2607-2670)
1 password ≥ 12; 2 generate 12 words; 3 keys on every chain; 4 HIBP check; 5 seal → vault; 6 POST /signup
(faucet coins sent by the node); 7 show the words while in the background: wait for coins (180 s), CLAIM the
tag (tag claim tx signed by the user's key, fee from the faucet gift), announce the messaging key (retry once
after 10 s), wait for both to be indexed. If the faucet refuses, the name stays unclaimed and the page says so.
- Password change: re-seal the same words; POST /account/vault {blob} (signed-in by the words' key) on THAT
  arcade only; others keep the old-password copy.

## Finding a vault (sign-in)
- GET https://<host>/signin/{tag}?here=1 → 200 {"tag","pubkey","address","blob"}; 20/300 s per IP → 429.
  Without ?here=1 it fans out itself and answers with "from","elsewhere":true.
- Hosts asked (_arcade_hosts, ≤16, https on 443): domains from on-chain INSTANCE announcements (type 9) and
  host parts of wss:// MESH announcements from the last 3 days; DNS names only (never IPs). "op me" hosts are
  NOT used. A wss host works because mesh/wsock.py proxies GET /signin/… on the tunnel port to the web app.
- No mesh frame carries vaults. A V1.0 arcade without a public DNS name/tunnel won't be found by originals:
  file the vault with a public original (POST /signup) for that.
