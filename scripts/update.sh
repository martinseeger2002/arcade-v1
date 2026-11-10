#!/usr/bin/env bash
# Arcade V1.0 updater for Linux: installs the newest release over the one you have.
#
#   curl -fsSL https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/update.sh | bash
#
# Your wallet, its 12 words, your @handle, messages and chains are kept: only the programs
# (arcaded, arcade, the app) and the service file are replaced. The service is stopped
# first (it saves what it was writing) and started again on the new version.
# Options after `bash -s --` go to the installer (install-linux.sh), as with install.sh.
set -euo pipefail

BIN="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
HOME_DIR="${ARCADE_HOME:-$DATA/arcade}"
OWNER_REPO="${ARCADE_GITHUB:-martinseeger2002/arcade-v1}"
BRANCH="${ARCADE_BRANCH:-main}"

say() { printf '\033[1m%s\033[0m\n' "$*"; }

if [ ! -x "$BIN/arcaded" ]; then
  echo "Arcade V1.0 isn't installed here yet. Install it with:" >&2
  echo "  curl -fsSL https://raw.githubusercontent.com/$OWNER_REPO/$BRANCH/scripts/install.sh | bash" >&2
  exit 1
fi

newest="$(curl -fsSL "https://api.github.com/repos/$OWNER_REPO/releases/latest" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1 || true)"
say "Updating Arcade V1.0${newest:+ to $newest}. Your wallet and data in $HOME_DIR stay as they are."
curl -fsSL "https://raw.githubusercontent.com/$OWNER_REPO/$BRANCH/scripts/install.sh" | ARCADE_NO_OPEN=1 bash -s -- "$@"
say "Updated. If the app was open, close it and open Arcade V1.0 again to load the new version."
