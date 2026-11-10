#!/usr/bin/env bash
# Arcade V1.0 uninstaller for Linux: removes EVERYTHING the installer put on this computer,
# including your data (wallet, its 12 words, imported wallets, messages, chains).
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/uninstall.sh | bash
#
# It asks you to type DELETE first (from the terminal). Without a terminal it does nothing
# unless ARCADE_UNINSTALL_YES=1 is set.
set -euo pipefail

DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
HOME_DIR="${ARCADE_HOME:-$DATA/arcade}"
BIN="$HOME/.local/bin"
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APPS="$DATA/applications"
ICON="$DATA/icons/hicolor/256x256/apps/arcade-v1.png"
SRC="${ARCADE_SRC:-$DATA/arcade-v1-src}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/arcade-v1"

say() { printf '\033[1m%s\033[0m\n' "$*"; }

# Never delete something that isn't an arcade data folder.
case "$HOME_DIR" in
  ""|"/"|"$HOME"|"$HOME/"|"$DATA"|"$DATA/") echo "Refusing: the data folder would be $HOME_DIR." >&2; exit 1 ;;
esac

say "This removes Arcade V1.0 completely from this computer:"
echo "  - the programs, the app and its menu entry, the background service"
echo "  - your data in $HOME_DIR$( [ -d "$HOME_DIR" ] && printf ' (%s)' "$(du -sh "$HOME_DIR" 2>/dev/null | cut -f1)" ):"
echo "    your wallet and its 12 words, imported wallets, messages, groups and the chains"
echo
if [ -e "$HOME_DIR/arcade.mdbx" ]; then
  say "If your wallet holds anything (coins, your @handle), write down your 12 words first:"
  echo "  $BIN/arcade --home $HOME_DIR wallet phrase"
  echo "Without them the wallet can't be restored once this is done."
  echo
fi

answer=""
if [ "${ARCADE_UNINSTALL_YES:-0}" = 1 ]; then
  answer=DELETE
elif [ -r /dev/tty ] && (exec </dev/tty) 2>/dev/null; then
  printf 'Type DELETE to remove everything (anything else cancels): '
  read -r answer </dev/tty || answer=""
else
  echo "No terminal to ask: nothing removed. (ARCADE_UNINSTALL_YES=1 removes without asking.)" >&2
  exit 1
fi
if [ "$answer" != DELETE ]; then
  echo "Cancelled: nothing removed."
  exit 0
fi

say "Removing Arcade V1.0..."
systemctl --user disable --now arcaded.service 2>/dev/null || true
pkill -x arcade-desktop 2>/dev/null || true
rm -f "$BIN/arcaded" "$BIN/arcade" "$BIN/arcade-desktop" "$UNITS/arcaded.service" "$APPS/arcade-v1.desktop" "$ICON"
systemctl --user daemon-reload 2>/dev/null || true
systemctl --user reset-failed arcaded.service 2>/dev/null || true
update-desktop-database "$APPS" 2>/dev/null || true
rm -rf "$HOME_DIR" "$SRC" "$CACHE"
say "Arcade V1.0 is removed, with its data. Installing again starts fresh (restore a wallet from its 12 words then)."
