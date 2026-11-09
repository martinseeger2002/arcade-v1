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

### Uninstall

```sh
systemctl --user disable --now arcaded
rm -f ~/.local/bin/arcaded ~/.local/bin/arcade ~/.local/bin/arcade-desktop \
      ~/.config/systemd/user/arcaded.service ~/.local/share/applications/arcade-v1.desktop \
      ~/.local/share/icons/hicolor/256x256/apps/arcade-v1.png
```

Your data in `~/.local/share/arcade` (wallet included) is left alone. Delete it yourself only after you've written down your 12 words.
