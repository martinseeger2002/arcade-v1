# Security

> **Arcade V1 is beta software with no independent security audit yet. Do not use funds you can't afford to lose. Do not restore a seed phrase that holds real savings. It installs a background service that starts at login and connects to other arcades.**

The same notice is at the top of the [README](README.md) and on a screen in the Windows installer (`scripts/windows/setup.nsi`). That screen does not continue until you check the box. No outside firm has audited this code. Treat it as beta: test coins, or amounts you can afford to lose. Do not type a seed phrase that holds real savings into it.

## What the service can access

`arcaded` is the background service. The installers start it as the user who installed it, and again at every login. They do not ask for an administrator password, and the service does not run as root or Administrator.

- **Linux.** A systemd user service, `arcaded.service` (`systemctl --user`). The unit sets `UMask=0077` and `NoNewPrivileges=yes`. The data folder `~/.local/share/arcade` is mode `700`. This is not a locked-down system unit: the process can read and write what your user can, including that folder.
- **Windows.** A Task Scheduler task named **Arcade V1.0** (or a Startup-folder shortcut if the task cannot be created). Data is in `%LOCALAPPDATA%\arcade`. Programs are in `%LOCALAPPDATA%\Programs\Arcade`. A firewall rule for the mesh and STUN ports is added only when the installer is run as administrator; otherwise Windows may ask the first time the service listens.
- **macOS.** A launchd agent that starts at login. Data is in `~/Library/Application Support/arcade`.

Inside the data folder:

- `secret.key` is created so only this user can read it: mode `0600` on Linux and macOS, and a user-only ACL on Windows (`arcade_app::write_private`). It is the key that encrypts secrets in `arcade.mdbx`.
- The 12-word recovery phrase is stored encrypted in that database. The service can decrypt it, because it holds `secret.key`. It is not sent to other arcades.
- Chain data, messages, and the mesh identity (`node_key`, also encrypted there) live in the same folder.

The wallet's HTTP commands answer only this computer by default. A request is treated as local when it comes from a loopback address, the `Host` header is `127.0.0.1`, `localhost`, or `[::1]`, and there are no proxy headers. `--behind-proxy` makes the web port count nothing as this computer, so those commands answer the local socket only. `--http-bind` can open the web port to other addresses; that is off unless you set it.

## Ports it opens

A port that is already taken moves to the next free one. `0` turns that port off.

| Port | Protocol | Where it listens | What it serves |
|---|---|---|---|
| **8420** | TCP | `127.0.0.1` by default (`--http-port`, `--http-bind`) | The app and its HTTP API, including the wallet. Other computers cannot open it unless you change the bind address. |
| (ephemeral) | TCP | `127.0.0.1` only | `arcade-signer`'s confirmation window, for approving a send from the browser. Not the mesh, and not bound to any other address. The session in that window's link is not given to arcaded. |
| **8421** | TCP | `0.0.0.0` (`--mesh-port`) | The mesh. Links are encrypted (X25519, then XChaCha20-Poly1305). Arcades exchange blocks, chain data, and signed records, including signed code releases. This port does not serve the wallet API or the 12 words. `--no-mesh` turns the links off. |
| **3478** | UDP | `0.0.0.0` (`--stun-port`) | STUN, so arcades can find a path to each other, and by default a TURN media relay on the same socket (`turnsvc`). The relay forwards DTLS/SRTP between two addresses a logged-in client named and cannot read that media. It is on unless `ARCADE_MESH_TURN=0` or the mesh setting `turn` is false. A month's relay traffic stops at 100 GB unless that limit is changed. It does not serve the wallet. |

If you do not want 8421 or 3478 reachable from the internet, do not forward them on your router. The service still dials out to other arcades unless `--no-mesh` is set.

## How updates are checked

Automatic updates are **off** on a fresh install. They run only after you turn them on (Options, or the `auto_update` setting `{"on": true}`).

When they are on, `arcaded` looks at releases other arcades already hold (the first look a few minutes after start, then about every 10 minutes). A release is installed only when all of these hold (`crates/arcaded/src/updater.rs`, `crates/arcaded/src/arccode.rs`):

- The release record is signed by a release key this arcade trusts. On the public network that is Strawberry's release key, pinned in `RELEASE_KEYS`. There is no switch that skips the signature check.
- Every file matches the SHA-256 in that signed record. The program archive is hashed again before it is unpacked, and a mismatch is not installed.
- The new `arcaded --version` names that same release.
- The release version is newer than the one that is running. An older version is not installed.
- This build is a release build. A copy you compiled yourself has no release version and never updates itself.

The previous programs are kept beside the new ones as `.prev` (on Windows they are renamed aside, because a running program cannot be replaced in place). Windows uses the same signed-release checks as Linux. The unpack folder is inside the data directory, mode `0700` on Unix, not the shared temporary folder.

## Reporting a security problem

Write to **team@arcade-nodes.com** before you post it in public, so it can be fixed first. Say what you ran, the version (`arcaded --version`), and what you saw. Do not include your 12 words, `secret.key`, or any private key.
