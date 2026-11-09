#!/usr/bin/env bash
# Arcade V1.0 one-line installer for Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash
#
# It does everything itself:
#   1. installs the build tools it needs with apt (asks for your password once) when any are missing;
#   2. installs Rust for this user if it is missing;
#   3. fetches (or updates) the source in ~/.local/share/arcade-v1-src (git if present, else a tarball);
#   4. runs scripts/install-linux.sh: builds the service, the CLI and the desktop app and installs them with the
#      app's icon and menu launcher.
# Options after `bash -s --` go to install-linux.sh, for example:
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash -s -- --no-desktop
set -euo pipefail

OWNER_REPO="${ARCADE_GITHUB:-martinseeger2002/arcade-v1}"
BRANCH="${ARCADE_BRANCH:-main}"
SRC="${ARCADE_SRC:-${XDG_DATA_HOME:-$HOME/.local/share}/arcade-v1-src}"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
# stdin is the curl pipe: anything that asks (sudo's password) must read the terminal instead.
TTY=/dev/null
if [ -r /dev/tty ] && (exec </dev/tty) 2>/dev/null; then TTY=/dev/tty; fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "This installer is for Linux." >&2
  exit 1
fi

# --- 1. build tools -------------------------------------------------------------------------------------------------
have_libclang() {
  ls /usr/lib/llvm-*/lib/libclang*.so* /usr/lib*/libclang*.so* /usr/lib/*-linux-gnu/libclang*.so* >/dev/null 2>&1
}
needs_tools() {
  ! command -v cc >/dev/null || ! command -v pkg-config >/dev/null || ! have_libclang \
    || ! pkg-config --exists webkit2gtk-4.1 gtk+-3.0 2>/dev/null
}
if needs_tools; then
  if command -v apt-get >/dev/null; then
    PKGS=(build-essential pkg-config clang libclang-dev curl ca-certificates)
    # The desktop app's GTK/WebKit files; older releases (Ubuntu 20.04, Mint 20) have no webkit2gtk-4.1, and the
    # app is then skipped while the rest installs.
    for p in libwebkit2gtk-4.1-dev libgtk-3-dev; do
      if apt-cache show "$p" >/dev/null 2>&1; then PKGS+=("$p"); fi
    done
    say "Installing the build tools (${PKGS[*]}). sudo will ask for your password:"
    if [ "$(id -u)" = 0 ]; then SUDO=(); else SUDO=(sudo); fi
    "${SUDO[@]}" apt-get update -q <"$TTY"
    "${SUDO[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y -q "${PKGS[@]}" <"$TTY"
  else
    echo "Install a C compiler, pkg-config, clang/libclang and (for the desktop app) webkit2gtk-4.1 + gtk3" >&2
    echo "development files with your package manager, then run this again." >&2
    if ! command -v cc >/dev/null || ! command -v pkg-config >/dev/null || ! have_libclang; then exit 1; fi
  fi
fi

# --- 2. Rust --------------------------------------------------------------------------------------------------------
if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  say "Installing Rust for this user (rustup)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"

# --- 3. the source --------------------------------------------------------------------------------------------------
mkdir -p "$(dirname "$SRC")"
if command -v git >/dev/null; then
  if [ -d "$SRC/.git" ]; then
    say "Updating the source in $SRC..."
    git -C "$SRC" fetch -q origin "$BRANCH"
    git -C "$SRC" checkout -q "$BRANCH"
    git -C "$SRC" reset -q --hard "origin/$BRANCH"
  else
    say "Fetching the source into $SRC..."
    rm -rf "$SRC"
    git clone -q --branch "$BRANCH" "https://github.com/$OWNER_REPO.git" "$SRC"
  fi
else
  say "Fetching the source into $SRC..."
  TMP="$(mktemp -d)"
  curl -fsSL "https://codeload.github.com/$OWNER_REPO/tar.gz/refs/heads/$BRANCH" | tar -xz -C "$TMP"
  # Keep the build folder across updates so a rebuild is quick.
  if [ -d "$SRC/target" ]; then mv "$SRC/target" "$TMP/target.keep"; fi
  rm -rf "$SRC"
  mv "$TMP"/*-"$BRANCH" "$SRC"
  if [ -d "$TMP/target.keep" ]; then mv "$TMP/target.keep" "$SRC/target"; fi
  rm -rf "$TMP"
fi

# --- 4. build and install -------------------------------------------------------------------------------------------
exec bash "$SRC/scripts/install-linux.sh" "$@" <"$TTY"
