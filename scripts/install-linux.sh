#!/usr/bin/env bash
# Arcade V1.0: build and install for the current user on Linux, no sudo needed for the install itself.
#
#   scripts/install-linux.sh [--prebuilt DIR] [--no-desktop] [--home DIR] [--http-port N] [--mesh-port N] [--stun-port N]
#
# What it does:
#   1. builds arcaded (the service), arcade (the CLI) and arcade-desktop (the app window), release mode -- or, with
#      --prebuilt DIR, takes them ready-made from DIR/bin (a release download, see .github/workflows/release.yml);
#   2. installs them in ~/.local/bin;
#   3. installs the app icon (crates/arcade-desktop/icons/icon.png) into the user's icon theme, and an
#      "Arcade V1.0" launcher in the app menu that opens the desktop app with that icon;
#   4. installs and starts arcaded as a user service (systemd --user), started at login and at boot with linger.
#
# Needs: Rust (https://rustup.rs), clang/libclang, pkg-config, and for the desktop app the GTK/WebKit build
# files (Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev). Without them the desktop app is
# skipped with a note, and the rest still installs (the CLI and every service feature work without it).
#
# Ports: arcaded's defaults are 8420 (http), 8421 (mesh) and 3478 (STUN). When one is already taken on this machine
# (another arcade runs here), the next free one is used: the http and mesh ports by counting up, STUN by turning off.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$HOME/.local/bin"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
HOME_DIR="${ARCADE_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/arcade}"
DESKTOP=1
PREBUILT=""
HTTP_PORT="" MESH_PORT="" STUN_PORT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-desktop) DESKTOP=0 ;;
    --prebuilt) PREBUILT="$(cd "$2" && pwd)"; shift ;;
    --home) HOME_DIR="$2"; shift ;;
    --http-port) HTTP_PORT="$2"; shift ;;
    --mesh-port) MESH_PORT="$2"; shift ;;
    --stun-port) STUN_PORT="$2"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '\033[1m%s\033[0m\n' "$*"; }
taken_tcp() { ss -Hltn "sport = :$1" 2>/dev/null | grep -q .; }
taken_udp() { ss -Hlun "sport = :$1" 2>/dev/null | grep -q .; }
free_tcp() { local p="$1"; while taken_tcp "$p"; do p=$((p + 10)); done; echo "$p"; }

if [ -n "$PREBUILT" ]; then
  # --- ready-made programs ------------------------------------------------------------------------------------------
  FROM="$PREBUILT/bin"
  ICON_SRC="$PREBUILT/icon.png"
  for b in arcaded arcade; do
    if [ ! -x "$FROM/$b" ] || ldd "$FROM/$b" 2>&1 | grep -q "not found"; then
      echo "The prebuilt $b does not run on this system (too old a Linux?). Build from source instead:" >&2
      echo "  ARCADE_BUILD=1 bash install.sh" >&2
      exit 3
    fi
  done
  if [ "$DESKTOP" = 1 ]; then
    if [ ! -x "$FROM/arcade-desktop" ]; then
      DESKTOP=0
    elif ldd "$FROM/arcade-desktop" 2>&1 | grep -q "not found"; then
      # The app window needs the system's web engine (WebKitGTK 4.1) and GTK 3 at run time, nothing to build.
      if command -v apt-get >/dev/null && apt-cache show libwebkit2gtk-4.1-0 >/dev/null 2>&1; then
        say "The app window needs the system web engine (libwebkit2gtk-4.1-0). sudo will ask for your password:"
        if [ "$(id -u)" = 0 ]; then SUDO=(); else SUDO=(sudo); fi
        "${SUDO[@]}" apt-get install -y -q libwebkit2gtk-4.1-0 libgtk-3-0 || true
      fi
      if ldd "$FROM/arcade-desktop" 2>&1 | grep -q "not found"; then
        say "The desktop app can't run here (no WebKitGTK 4.1); skipping it. Everything else installs."
        DESKTOP=0
      fi
    fi
  fi
else
  # --- the toolchain --------------------------------------------------------------------------------------------------
  CARGO="$(command -v cargo || true)"
  [ -z "$CARGO" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO="$HOME/.cargo/bin/cargo"
  if [ -z "$CARGO" ]; then
    echo "Rust is not installed. Install it with:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
  fi
  # mdbx-sys makes its bindings with libclang, which cannot always find the compiler's own headers (stdarg.h).
  if [ -z "${LIBCLANG_PATH:-}" ]; then
    for d in /usr/lib/llvm-*/lib /usr/lib64 /usr/lib/x86_64-linux-gnu /usr/lib/aarch64-linux-gnu; do
      if ls "$d"/libclang*.so* >/dev/null 2>&1; then export LIBCLANG_PATH="$d"; fi
    done
  fi
  if [ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ] && command -v gcc >/dev/null; then
    export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"
  fi

  PKGS=(-p arcaded -p arcade-cli)
  if [ "$DESKTOP" = 1 ]; then
    if pkg-config --exists webkit2gtk-4.1 gtk+-3.0 2>/dev/null; then
      PKGS+=(-p arcade-desktop)
    else
      say "The desktop app needs the GTK/WebKit build files; skipping it. To add it later:"
      echo "  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev   # then run this script again"
      DESKTOP=0
    fi
  fi

  # --- build ----------------------------------------------------------------------------------------------------------
  say "Building ${PKGS[*]//-p /} (release)..."
  (cd "$ROOT" && "$CARGO" build --release "${PKGS[@]}")
  FROM="$ROOT/target/release"
  ICON_SRC="$ROOT/crates/arcade-desktop/icons/icon.png"
fi

# --- install --------------------------------------------------------------------------------------------------------
mkdir -p "$BIN" "$HOME_DIR"
# Stop the service first so its binary can be replaced.
systemctl --user stop arcaded.service 2>/dev/null || true
install -m 0755 "$FROM/arcaded" "$BIN/arcaded"
install -m 0755 "$FROM/arcade" "$BIN/arcade"

if [ "$DESKTOP" = 1 ]; then
  install -m 0755 "$FROM/arcade-desktop" "$BIN/arcade-desktop"
  mkdir -p "$ICONS" "$APPS"
  install -m 0644 "$ICON_SRC" "$ICONS/arcade-v1.png"
  # WebKitGTK draws a blank window on some NVIDIA drivers unless its DMA-BUF renderer is off.
  ENV_LINE="ARCADE_HOME=$HOME_DIR"
  [ -e /proc/driver/nvidia ] && ENV_LINE="$ENV_LINE WEBKIT_DISABLE_DMABUF_RENDERER=1"
  cat > "$APPS/arcade-v1.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Arcade V1.0
Comment=Arcade V1.0 — Pepecoin testnet, mesh and @handles
Exec=env $ENV_LINE $BIN/arcade-desktop
Icon=$ICONS/arcade-v1.png
Terminal=false
Categories=Network;Finance;
StartupWMClass=arcade-desktop
EOF
  update-desktop-database "$APPS" 2>/dev/null || true
  gtk-update-icon-cache -q "${ICONS%/256x256/apps}" 2>/dev/null || true
fi

# --- the service ----------------------------------------------------------------------------------------------------
[ -z "$HTTP_PORT" ] && HTTP_PORT="$(free_tcp 8420)"
[ -z "$MESH_PORT" ] && MESH_PORT="$(free_tcp 8421)"
if [ "$MESH_PORT" = "$HTTP_PORT" ]; then MESH_PORT="$(free_tcp $((HTTP_PORT + 1)))"; fi
if [ -z "$STUN_PORT" ]; then
  if taken_udp 3478; then STUN_PORT=0; else STUN_PORT=3478; fi
fi
mkdir -p "$UNITS"
cat > "$UNITS/arcaded.service" <<EOF
[Unit]
Description=Arcade V1.0 (chain sync, mesh, tags)
After=network-online.target

[Service]
Type=simple
Environment=ARCADE_HOME=$HOME_DIR
ExecStart=$BIN/arcaded --home $HOME_DIR --http-port $HTTP_PORT --mesh-port $MESH_PORT --stun-port $STUN_PORT
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now arcaded.service

say "Arcade V1.0 is installed and running."
echo "  data:     $HOME_DIR"
echo "  ports:    http $HTTP_PORT, mesh $MESH_PORT, STUN $([ "$STUN_PORT" = 0 ] && echo off || echo "$STUN_PORT")"
echo "  service:  systemctl --user status arcaded"
[ "$DESKTOP" = 1 ] && echo "  app:      \"Arcade V1.0\" in your app menu (or run arcade-desktop)"
echo "  setup:    arcade --home $HOME_DIR setup     (wallet and @handle)"
if [ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" != "yes" ]; then
  echo
  echo "To keep it running when you are logged out (and start it at boot):  sudo loginctl enable-linger $USER"
fi
case ":$PATH:" in *":$BIN:"*) ;; *) echo; echo "Add $BIN to your PATH to run arcade from any terminal." ;; esac
