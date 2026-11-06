#!/bin/sh
# Recreate vendor/webrtc-sys: webrtc-sys from crates.io (checked by SHA-256)
# plus vendor/webrtc-sys.patch. Cargo needs it before any build; the Makefile
# runs this for you. See vendor/README.md.
set -eu
cd "$(dirname "$0")/.."

VERSION=0.3.52
SHA256=bae66695bde7b250a9e5f7976db6015e30fa06033afc1745e5499d55aa51612a
DIR=vendor/webrtc-sys
PATCH=vendor/webrtc-sys.patch
STAMP="$DIR/.quipcall-patch"
LOCK=vendor/.webrtc-sys.lock

want=$(shasum -a 256 "$PATCH" | cut -d' ' -f1)
current() { [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$want" ]; }
if current; then
	exit 0
fi

# One rebuild at a time: two builds starting together would otherwise
# delete each other's files.
waited=0
until mkdir "$LOCK" 2>/dev/null; do
	if [ "$waited" -ge 120 ]; then
		echo "Waited 2 minutes for $LOCK; remove it if no other build is running." >&2
		exit 1
	fi
	sleep 1
	waited=$((waited + 1))
done
tmp=$(mktemp -d vendor/.webrtc-sys.XXXXXX)
trap 'rm -rf "$tmp"; rmdir "$LOCK"' EXIT
trap 'exit 1' INT TERM
if current; then
	exit 0
fi

crate=$(ls ~/.cargo/registry/cache/*/webrtc-sys-$VERSION.crate 2>/dev/null | head -1 || true)
if [ -z "$crate" ]; then
	crate="$tmp/webrtc-sys.crate"
	curl -sSfL "https://static.crates.io/crates/webrtc-sys/webrtc-sys-$VERSION.crate" -o "$crate"
fi
echo "$SHA256  $crate" | shasum -a 256 -c - >/dev/null

# Build it next to the old one, then swap it in whole.
mkdir "$tmp/src"
tar -xzf "$crate" -C "$tmp/src" --strip-components 1
rm -f "$tmp/src/.cargo_vcs_info.json"
patch -s -d "$tmp/src" -p1 <"$PATCH"
echo "$want" >"$tmp/src/.quipcall-patch"
rm -rf "$DIR"
mv "$tmp/src" "$DIR"
echo "vendor/webrtc-sys: webrtc-sys $VERSION + $PATCH"
