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

want=$(shasum -a 256 "$PATCH" | cut -d' ' -f1)
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$want" ]; then
	exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
crate=$(ls ~/.cargo/registry/cache/*/webrtc-sys-$VERSION.crate 2>/dev/null | head -1 || true)
if [ -z "$crate" ]; then
	crate="$tmp/webrtc-sys.crate"
	curl -sSfL "https://static.crates.io/crates/webrtc-sys/webrtc-sys-$VERSION.crate" -o "$crate"
fi
echo "$SHA256  $crate" | shasum -a 256 -c - >/dev/null

rm -rf "$DIR"
mkdir -p "$DIR"
tar -xzf "$crate" -C "$DIR" --strip-components 1
rm -f "$DIR/.cargo_vcs_info.json"
patch -s -d "$DIR" -p1 <"$PATCH"
echo "$want" >"$STAMP"
echo "vendor/webrtc-sys: webrtc-sys $VERSION + $PATCH"
