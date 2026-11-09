#!/usr/bin/env bash
# Arcade V1.0 one-line installer for Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash
#
# Normally it downloads the newest prebuilt release for this PC (x86_64 or aarch64; built by
# .github/workflows/release.yml), checks its sha256 and installs it: nothing to compile, no build tools.
# It builds from source instead only when there is no download for this PC, when the download doesn't run here
# (an older Linux), or when you ask with ARCADE_BUILD=1. Building installs the build tools with apt (asks for your
# password once) and Rust for this user.
#
# Options after `bash -s --` go to install-linux.sh, for example:
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/install.sh | bash -s -- --no-desktop
set -euo pipefail

OWNER_REPO="${ARCADE_GITHUB:-martinseeger2002/arcade-v1}"
BRANCH="${ARCADE_BRANCH:-main}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
SRC="${ARCADE_SRC:-$DATA/arcade-v1-src}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/arcade-v1"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
# stdin is the curl pipe: anything that asks (sudo's password) must read the terminal instead.
TTY=/dev/null
if [ -r /dev/tty ] && (exec </dev/tty) 2>/dev/null; then TTY=/dev/tty; fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "This installer is for Linux." >&2
  exit 1
fi

# --- the quick way: a prebuilt release ------------------------------------------------------------------------------
prebuilt() {
  local arch name url dir
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) return 1 ;;
  esac
  name="arcade-v1-linux-$arch"
  url="https://github.com/$OWNER_REPO/releases/latest/download/$name.tar.gz"
  say "Downloading Arcade V1.0 for $arch..."
  rm -rf "$CACHE" && mkdir -p "$CACHE"
  curl -fsSL "$url" -o "$CACHE/$name.tar.gz" || { echo "No prebuilt download for this PC yet."; return 1; }
  curl -fsSL "$url.sha256" -o "$CACHE/$name.tar.gz.sha256" || { echo "The download has no checksum."; return 1; }
  (cd "$CACHE" && sha256sum -c --quiet "$name.tar.gz.sha256") || { echo "The download is damaged (checksum)." >&2; return 1; }
  tar -xzf "$CACHE/$name.tar.gz" -C "$CACHE"
  dir="$CACHE/$name"
  [ -f "$dir/COMMIT" ] && echo "  version $(cut -c1-12 "$dir/COMMIT")"
  bash "$dir/scripts/install-linux.sh" --prebuilt "$dir" "$@" <"$TTY"
}

# --- the slow way: build from source --------------------------------------------------------------------------------
have_libclang() {
  ls /usr/lib/llvm-*/lib/libclang*.so* /usr/lib*/libclang*.so* /usr/lib/*-linux-gnu/libclang*.so* >/dev/null 2>&1
}
needs_tools() {
  ! command -v cc >/dev/null || ! command -v pkg-config >/dev/null || ! have_libclang \
    || ! pkg-config --exists webkit2gtk-4.1 gtk+-3.0 2>/dev/null
}
from_source() {
  say "Building Arcade V1.0 from source (the first build takes 10-20 minutes)..."
  if needs_tools; then
    if command -v apt-get >/dev/null; then
      local pkgs=(build-essential pkg-config clang libclang-dev curl ca-certificates) p
      # Older releases (Ubuntu 20.04, Mint 20) have no webkit2gtk-4.1: the app is then skipped, the rest installs.
      for p in libwebkit2gtk-4.1-dev libgtk-3-dev; do
        if apt-cache show "$p" >/dev/null 2>&1; then pkgs+=("$p"); fi
      done
      say "Installing the build tools (${pkgs[*]}). sudo will ask for your password:"
      local sudo=(sudo); [ "$(id -u)" = 0 ] && sudo=()
      "${sudo[@]}" apt-get update -q <"$TTY"
      "${sudo[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y -q "${pkgs[@]}" <"$TTY"
    else
      echo "Install a C compiler, pkg-config, clang/libclang and (for the desktop app) webkit2gtk-4.1 + gtk3" >&2
      echo "development files with your package manager, then run this again." >&2
      if ! command -v cc >/dev/null || ! command -v pkg-config >/dev/null || ! have_libclang; then exit 1; fi
    fi
  fi
  if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    say "Installing Rust for this user (rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
  fi
  export PATH="$HOME/.cargo/bin:$PATH"

  mkdir -p "$(dirname "$SRC")"
  if command -v git >/dev/null; then
    if [ -d "$SRC/.git" ]; then
      git -C "$SRC" fetch -q origin "$BRANCH"
      git -C "$SRC" checkout -q "$BRANCH"
      git -C "$SRC" reset -q --hard "origin/$BRANCH"
    else
      rm -rf "$SRC"
      git clone -q --branch "$BRANCH" "https://github.com/$OWNER_REPO.git" "$SRC"
    fi
  else
    local tmp; tmp="$(mktemp -d)"
    curl -fsSL "https://codeload.github.com/$OWNER_REPO/tar.gz/refs/heads/$BRANCH" | tar -xz -C "$tmp"
    if [ -d "$SRC/target" ]; then mv "$SRC/target" "$tmp/target.keep"; fi   # keep the build: updates are quick
    rm -rf "$SRC"
    mv "$tmp"/*-"$BRANCH" "$SRC"
    if [ -d "$tmp/target.keep" ]; then mv "$tmp/target.keep" "$SRC/target"; fi
    rm -rf "$tmp"
  fi
  bash "$SRC/scripts/install-linux.sh" "$@" <"$TTY"
}

if [ "${ARCADE_BUILD:-0}" != 1 ]; then
  set +e
  prebuilt "$@"
  code=$?
  set -e
  [ "$code" = 0 ] && exit 0
  # 3: the prebuilt programs don't run on this Linux. Anything else before installing: no usable download.
  say "Falling back to building from source."
fi
from_source "$@"
