#!/usr/bin/env bash
# Arcade V1.0 one-line installer for Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash
#
# Fetches (or updates) the source in ~/.local/share/arcade-v1-src, installs Rust for this user if it is missing,
# then runs scripts/install-linux.sh, which builds the service, the CLI and the desktop app and installs them with
# the app's icon and menu launcher. Options after `bash -s --` go to install-linux.sh, for example:
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash -s -- --no-desktop
set -euo pipefail

REPO="${ARCADE_REPO:-https://github.com/martinseeger2002/arcade-v1.git}"
BRANCH="${ARCADE_BRANCH:-main}"
SRC="${ARCADE_SRC:-${XDG_DATA_HOME:-$HOME/.local/share}/arcade-v1-src}"

say() { printf '\033[1m%s\033[0m\n' "$*"; }

if [ "$(uname -s)" != "Linux" ]; then
  echo "This installer is for Linux." >&2
  exit 1
fi

missing=()
for tool in git cc pkg-config; do command -v "$tool" >/dev/null || missing+=("$tool"); done
if ! ls /usr/lib/llvm-*/lib/libclang*.so* /usr/lib*/libclang*.so* /usr/lib/*-linux-gnu/libclang*.so* >/dev/null 2>&1; then
  missing+=("libclang")
fi
if [ ${#missing[@]} -gt 0 ]; then
  echo "Missing: ${missing[*]}. On Debian/Ubuntu install the build tools with:" >&2
  echo "  sudo apt install -y git build-essential pkg-config clang libclang-dev libwebkit2gtk-4.1-dev libgtk-3-dev" >&2
  exit 1
fi

if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  say "Installing Rust for this user (rustup)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"

if [ -d "$SRC/.git" ]; then
  say "Updating the source in $SRC..."
  git -C "$SRC" fetch -q origin "$BRANCH"
  git -C "$SRC" checkout -q "$BRANCH"
  git -C "$SRC" reset -q --hard "origin/$BRANCH"
else
  say "Fetching the source into $SRC..."
  mkdir -p "$(dirname "$SRC")"
  git clone -q --branch "$BRANCH" "$REPO" "$SRC"
fi

# stdin is the curl pipe: give the installer the terminal instead, if there is one.
if [ -r /dev/tty ]; then
  exec bash "$SRC/scripts/install-linux.sh" "$@" </dev/tty
else
  exec bash "$SRC/scripts/install-linux.sh" "$@"
fi
