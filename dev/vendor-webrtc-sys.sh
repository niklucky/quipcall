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

# One rebuild at a time, under a lock the OS releases when its holder dies
# (perl's flock: macOS has no flock(1)). Two builds starting together would
# otherwise delete each other's files.
if [ -z "${QUIPCALL_VENDOR_LOCKED:-}" ]; then
	exec perl -MFcntl=:flock -e '
		open(my $lock, ">>", shift) or die "lock: $!";
		flock($lock, LOCK_EX) or die "lock: $!";
		system(@ARGV);
		exit($? == -1 ? 1 : ($? & 127 ? 128 + ($? & 127) : $? >> 8));
	' "$LOCK" env QUIPCALL_VENDOR_LOCKED=1 "$0"
fi
if current; then
	exit 0
fi

tmp=$(mktemp -d vendor/.webrtc-sys.XXXXXX)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM

crate=$(ls ~/.cargo/registry/cache/*/webrtc-sys-$VERSION.crate 2>/dev/null | head -1 || true)
if [ -z "$crate" ]; then
	crate="$tmp/webrtc-sys.crate"
	curl -sSfL "https://static.crates.io/crates/webrtc-sys/webrtc-sys-$VERSION.crate" -o "$crate"
fi
echo "$SHA256  $crate" | shasum -a 256 -c - >/dev/null

mkdir "$tmp/src"
tar -xzf "$crate" -C "$tmp/src" --strip-components 1
rm -f "$tmp/src/.cargo_vcs_info.json"
patch -s -d "$tmp/src" -p1 <"$PATCH"
echo "$want" >"$tmp/src/.quipcall-patch"
# Swap whole trees by renaming, so a run killed here leaves a complete tree,
# or none (rebuilt next time); the old one goes with $tmp.
if [ -e "$DIR" ]; then
	mv "$DIR" "$tmp/old"
fi
mv "$tmp/src" "$DIR"
echo "vendor/webrtc-sys: webrtc-sys $VERSION + $PATCH"
