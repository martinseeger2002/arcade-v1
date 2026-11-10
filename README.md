# Arcade V1.0

A from-scratch Rust rebuild of Dogecoin Arcade, wire-compatible with the existing arcades. Pepecoin testnet first; other chains are add-ons. See [docs/PLAN.md](docs/PLAN.md).

## Install on Linux

Paste this one command into a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash
```

That's all. It downloads the newest ready-made release for your PC (Intel/AMD or ARM), checks its checksum, and installs it in seconds, with nothing to compile. It installs:

- **arcaded**, the arcade service, started now and at every login (`systemctl --user status arcaded`);
- **arcade**, the command line (`arcade status`, `arcade setup`, ...);
- **Arcade V1.0**, the desktop app, with its icon and a launcher in your app menu.

Everything goes in your home folder: programs in `~/.local/bin`, data in `~/.local/share/arcade`. Run the same command again to update.

### Requirements

The ready-made downloads run on Ubuntu 22.04 or newer, Linux Mint 21 or newer, Debian 12 or newer, and similar. The app window uses the system's web engine (`libwebkit2gtk-4.1-0`). Most desktops already have it; if yours doesn't, the installer adds it with apt, asking for your password once.

### Building from source instead

It builds from source by itself when there's no download for your PC or the download doesn't run on your Linux. To force a build, run it as `... | ARCADE_BUILD=1 bash`.

A build installs what it needs with apt (`build-essential pkg-config clang libclang-dev libwebkit2gtk-4.1-dev libgtk-3-dev`, asking for your password once) and Rust for your user. The first build takes 10 to 20 minutes on an older laptop. On older releases without `libwebkit2gtk-4.1` (Ubuntu 20.04, Mint 20), it skips the desktop app and installs the rest.

### Making a release

Push a tag such as `v0.1.1`, or run the **release** workflow on GitHub (Actions → release → Run workflow). It builds for x86_64 and aarch64 on Ubuntu 22.04 and attaches `arcade-v1-linux-<arch>.tar.gz` and its `.sha256` to the release. The installer always takes the latest release.

### After it's installed

Open **Arcade V1.0** from your app menu, or run `arcade setup` in a terminal. Either way you then:

- make a new wallet, or restore one from its 12 words;
- claim your @handle.

The service connects to the mesh and syncs Pepecoin testnet by itself.

### Update

```sh
curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/update.sh | bash
```

This installs the newest release over the one you have. Your wallet, its 12 words, your @handle, messages and chains are kept: only the programs and the service file are replaced. The service is stopped first (it finishes what it was writing) and started again on the new version. If the app was open, close it and open it again.

### Options

Pass options after `bash -s --`:

```sh
curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash -s -- --no-desktop
```

| Option | Meaning |
|---|---|
| `--no-desktop` | Don't build the desktop app. |
| `--home DIR` | Data folder (default `~/.local/share/arcade`). |
| `--http-port N`, `--mesh-port N`, `--stun-port N` | Ports. The defaults are 8420, 8421 and 3478. A port that's already taken (for example, by another arcade on the same PC) moves to the next free one. STUN turns off (`0`) if its port is taken. |

To keep the service running while you're logged out, and to start it at boot: `sudo loginctl enable-linger $USER`.

From a clone of this repo, `scripts/install-linux.sh` does the same build and install.

### Tuning the sync (environment variables)

Set these in the service's environment, for example with `systemctl --user edit arcaded` and an `Environment=` line, or before `arcaded` when you run it by hand.

| Variable | Meaning |
|---|---|
| `ARCADE_COIN_CACHE_MB` | Coin cache while catching up a chain, in MB (default 0 = off). 1024 cuts the coin writes roughly in half on busy chains such as Dogecoin. |
| `ARCADE_MALLOC_ARENAS` | How many glibc malloc arenas arcaded may use (default 4). More arenas means less waiting between threads but more memory held. A `MALLOC_ARENA_MAX` you set yourself takes precedence. Freed memory is given back to the system once a minute. |
| `ARCADE_NICE` | Run the service at a lower CPU and disk priority, e.g. 10, so games and the desktop stay smooth while chains catch up (default: normal priority). |
| `ARCADE_COMMIT_TXS` | Transactions per database commit while catching up (default 20000). |
| `ARCADE_SYNC_TIMES` | Any value: log how long each part of every sync batch takes. |

Stopping the service writes the coin cache to disk first, which can take a few minutes while a chain is catching up. The service units allow 300 s for that (`TimeoutStopSec=300`). A unit written by an older installer lacks this line, so systemd kills the service after 90 s and the next start redoes the unsaved blocks. Re-run the installer, or add `TimeoutStopSec=300` under `[Service]` with `systemctl --user edit arcaded`.

### Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/uninstall.sh | bash
```

This removes everything: the programs, the app and its menu entry, the background service, **and your data in `~/.local/share/arcade`**: your wallet and its 12 words, imported wallets, messages and chains. It asks you to type `DELETE` first. If your wallet holds anything, write down your 12 words before (`arcade --home ~/.local/share/arcade wallet phrase`); on a new install you restore it from them on the Home tab.
