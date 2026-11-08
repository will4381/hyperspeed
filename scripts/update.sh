#!/bin/sh
# Rebuilds Hyperspeed from GitHub and swaps it in. Run by the app's Update button with HS_REPO set.
set -e
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

DATA="$HOME/Library/Application Support/Hyperspeed"
SRC="$DATA/source"
APP="/Applications/Hyperspeed.app"

echo running > "$DATA/update.status"
trap 'echo failed > "$DATA/update.status"' EXIT

if [ -d "$SRC/.git" ]; then
  git -C "$SRC" fetch origin main
  git -C "$SRC" reset --hard FETCH_HEAD
else
  rm -rf "$SRC"
  git clone "$HS_REPO" "$SRC"
fi

"$SRC/scripts/bundle.sh"

# The window goes first, so it can't respawn the old daemon during the restart.
pkill -TERM -f "$APP/Contents/MacOS/hyperspeed\$" || true
"$APP/Contents/MacOS/hyperspeed" restart-daemon
open -a "$APP"

trap - EXIT
echo done > "$DATA/update.status"
