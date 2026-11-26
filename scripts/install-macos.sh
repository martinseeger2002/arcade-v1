#!/usr/bin/env bash
# Arcade V1.0 for macOS (Apple silicon or Intel), for the current user, no administrator needed.
#
#   scripts/install-macos.sh [--prebuilt DIR] [--no-desktop] [--home DIR] [--http-port N] [--mesh-port N] [--stun-port N]
#
# What it does:
#   1. takes arcaded, arcade and arcade-desktop from the release archive it sits in (arcade-v1-macos-<arch>.tar.gz),
#      from --prebuilt DIR, or downloads the newest release's archive and checks its sha256;
#   2. installs them in ~/.local/bin (the updater replaces them there);
#   3. installs "Arcade V1.0.app" in ~/Applications, which opens the desktop app with its icon (camera and
#      microphone asked for apps that use them);
#   4. installs and starts arcaded as a launchd agent (com.dogecoinarcade.arcaded), started at login, at low priority
#      (ARCADE_NICE=10). Logs: ~/Library/Logs/arcaded.log.
# Data: ~/Library/Application Support/arcade (or --home / ARCADE_HOME).
#
# The programs are not signed or notarized yet: an archive downloaded with a browser is quarantined by macOS, so this
# removes the quarantine flag from the files it installs (a download by this script or curl has none).
set -euo pipefail

OWNER_REPO="${ARCADE_GITHUB:-martinseeger2002/arcade-v1}"
LABEL="com.dogecoinarcade.arcaded"
BIN="$HOME/.local/bin"
APP="$HOME/Applications/Arcade V1.0.app"
AGENTS="$HOME/Library/LaunchAgents"
PLIST="$AGENTS/$LABEL.plist"
LOGS="$HOME/Library/Logs"
HOME_DIR="${ARCADE_HOME:-$HOME/Library/Application Support/arcade}"
NICE="${ARCADE_NICE:-10}"
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
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '\033[1m%s\033[0m\n' "$*"; }
taken_tcp() { lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1; }
taken_udp() { lsof -nP -iUDP:"$1" >/dev/null 2>&1; }
free_tcp() { local p="$1" n=0; while taken_tcp "$p" && [ "$n" -lt 20 ]; do p=$((p + 10)); n=$((n + 1)); done; echo "$p"; }
xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }

[ "$(uname -s)" = Darwin ] || { echo "this installer is for macOS (Linux: scripts/install.sh)" >&2; exit 1; }
case "$(uname -m)" in
  arm64) ARCH=aarch64 ;;
  x86_64) ARCH=x86_64 ;;
  *) echo "no macOS build for $(uname -m)" >&2; exit 1 ;;
esac
NAME="arcade-v1-macos-$ARCH"

# Where the programs come from: --prebuilt, the archive this script is in, or a download.
SRC=""
if [ -n "$PREBUILT" ]; then
  SRC="$PREBUILT"
elif [ -x "$(dirname "${BASH_SOURCE[0]}")/../bin/arcaded" ]; then
  SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
else
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  url="https://github.com/$OWNER_REPO/releases/latest/download/$NAME.tar.gz"
  say "Downloading Arcade V1.0 for macOS ($ARCH)..."
  curl -fsSL "$url" -o "$TMP/$NAME.tar.gz" || { echo "No macOS download in the newest release yet." >&2; exit 1; }
  curl -fsSL "$url.sha256" -o "$TMP/$NAME.tar.gz.sha256"
  want="$(awk '{print $1}' "$TMP/$NAME.tar.gz.sha256")"
  got="$(shasum -a 256 "$TMP/$NAME.tar.gz" | awk '{print $1}')"
  [ "$want" = "$got" ] || { echo "Nothing was installed: the download is damaged (checksum)." >&2; exit 1; }
  tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"
  SRC="$TMP/$NAME"
fi
[ -x "$SRC/bin/arcaded" ] || { echo "no bin/arcaded in $SRC" >&2; exit 1; }
[ -f "$SRC/COMMIT" ] && echo "  version $(cut -c1-12 "$SRC/COMMIT")"

# Stop a running one first (arcaded writes its coin caches on SIGTERM; ExitTimeOut below gives it 5 minutes).
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true

mkdir -p "$BIN" "$HOME_DIR" "$AGENTS" "$LOGS"
chmod 700 "$HOME_DIR"
progs="arcaded arcade arcade-signer"
[ "$DESKTOP" = 1 ] && [ -x "$SRC/bin/arcade-desktop" ] && progs="$progs arcade-desktop"
for p in $progs; do
  install -m 755 "$SRC/bin/$p" "$BIN/$p.new"
  xattr -d com.apple.quarantine "$BIN/$p.new" 2>/dev/null || true
  mv -f "$BIN/$p.new" "$BIN/$p"
done

[ -n "$HTTP_PORT" ] || HTTP_PORT="$(free_tcp 8420)"
[ -n "$MESH_PORT" ] || MESH_PORT="$(free_tcp 8421)"
[ "$MESH_PORT" != "$HTTP_PORT" ] || MESH_PORT="$(free_tcp $((HTTP_PORT + 1)))"
if [ -z "$STUN_PORT" ]; then
  if taken_udp 3478; then STUN_PORT=0; else STUN_PORT=3478; fi
fi

# The launchd agent: arcaded at login, kept running unless it stopped cleanly, at background priority.
{
  cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$(printf '%s' "$BIN/arcaded" | xml)</string>
    <string>--home</string><string>$(printf '%s' "$HOME_DIR" | xml)</string>
    <string>--http-port</string><string>$HTTP_PORT</string>
    <string>--mesh-port</string><string>$MESH_PORT</string>
    <string>--stun-port</string><string>$STUN_PORT</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>ARCADE_HOME</key><string>$(printf '%s' "$HOME_DIR" | xml)</string>
    <key>ARCADE_NICE</key><string>$NICE</string>
  </dict>
  <key>WorkingDirectory</key><string>$(printf '%s' "$HOME_DIR" | xml)</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>ExitTimeOut</key><integer>300</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>$(printf '%s' "$LOGS/arcaded.log" | xml)</string>
  <key>StandardOutPath</key><string>$(printf '%s' "$LOGS/arcaded.log" | xml)</string>
</dict>
</plist>
EOF
} > "$PLIST"
plutil -lint "$PLIST" >/dev/null
launchctl bootstrap "gui/$(id -u)" "$PLIST"

# The app: a small bundle whose program opens ~/.local/bin/arcade-desktop (so an update of the programs updates the
# window too), with the icon and the camera/microphone reasons.
if [[ " $progs " == *" arcade-desktop "* ]]; then
  rm -rf "$APP"
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
  [ -f "$SRC/icon.icns" ] && cp "$SRC/icon.icns" "$APP/Contents/Resources/icon.icns"
  cat > "$APP/Contents/MacOS/Arcade" <<EOF
#!/bin/sh
export ARCADE_HOME="$HOME_DIR"
exec "$BIN/arcade-desktop" "\$@"
EOF
  chmod 755 "$APP/Contents/MacOS/Arcade"
  cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Arcade V1.0</string>
  <key>CFBundleDisplayName</key><string>Arcade V1.0</string>
  <key>CFBundleIdentifier</key><string>com.dogecoinarcade.arcade</string>
  <key>CFBundleExecutable</key><string>Arcade</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSCameraUsageDescription</key><string>Apps you open in Arcade may use the camera for video in their rooms, after you turn on Camera &amp; microphone for that app.</string>
  <key>NSMicrophoneUsageDescription</key><string>Apps you open in Arcade may use the microphone for voice in their rooms, after you turn on Camera &amp; microphone for that app.</string>
</dict>
</plist>
EOF
  xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
fi

stun_text="$STUN_PORT"; [ "$STUN_PORT" = 0 ] && stun_text=off
say "Arcade V1.0 is installed and running."
echo
say "Arcade V1.0 is in beta: testing is happening now, and things can change or break (test coins have no value)."
say "Thank you for trying it out! Questions, problems or ideas: team@arcade-nodes.com"
echo
echo "  programs: $BIN"
echo "  data:     $HOME_DIR"
echo "  ports:    http $HTTP_PORT, mesh $MESH_PORT, STUN $stun_text"
echo "  service:  launchd agent $LABEL (starts at login; logs in $LOGS/arcaded.log)"
[ -d "$APP" ] && echo "  app:      Arcade V1.0 in ~/Applications"
echo "  setup:    the app's Home tab walks you through it (or: arcade setup)"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "  add $BIN to your PATH to run \"arcade\" from a terminal." ;; esac
