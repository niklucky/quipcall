#!/bin/sh
# Install or update Quipcall:
#   curl -fsSL https://quipcall.com/download/install.sh | sh
# Downloaded with curl, the app carries no quarantine flag, so macOS opens it
# without the "unidentified developer" detour.
set -eu
# hw.optional.arm64 is 1 on Apple Silicon, even in a Terminal running under Rosetta.
if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
	URL=https://quipcall.com/download/Quipcall-apple-silicon.zip
else
	URL=https://quipcall.com/download/Quipcall-intel.zip
fi

dest=/Applications
[ -w "$dest" ] || dest=$HOME/Applications
mkdir -p "$dest"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading Quipcall…"
curl -fL --progress-bar "$URL" -o "$tmp/Quipcall.zip"
ditto -x -k "$tmp/Quipcall.zip" "$tmp"

if pgrep -xq Quipcall; then
	echo "Quitting the running Quipcall…"
	pkill -x Quipcall || true
	sleep 1
fi
rm -rf "$dest/Quipcall.app"
mv "$tmp/Quipcall.app" "$dest/"
xattr -dr com.apple.quarantine "$dest/Quipcall.app" 2>/dev/null || true

echo "Installed $dest/Quipcall.app"
open "$dest/Quipcall.app"
