#!/bin/sh
# Build Quipcall.app for Apple Silicon and for Intel, signed and zipped:
# target/mac/Quipcall-apple-silicon.zip and target/mac/Quipcall-intel.zip.
# One app per architecture: a universal binary would be ~60 MB, over budget.
#
#   deploy/mac/package.sh            build only
#   deploy/mac/package.sh --upload   build and publish at https://quipcall.com/download/
#
# QUIPCALL_BUILD overrides the commit-count build number for an uncommitted release.
# QUIPCALL_SERVER picks the server the app talks to (default https://quipcall.com).
# The signing identity lives in QUIPCALL_SIGNING_KEYCHAIN (made on first use,
# see signing.sh).
set -eu
cd "$(dirname "$0")/../.."

HOST=${QUIPCALL_HOST:-root@testron.dev}
export QUIPCALL_SERVER=${QUIPCALL_SERVER:-https://quipcall.com}
# ScreenCaptureKit arrived in 12.3. Below 10.14.4 the linker would also point
# Swift overlays (libswiftCoreMedia) at @rpath, which doesn't exist in the app.
export MACOSX_DEPLOYMENT_TARGET=12.3

KEYCHAIN=${QUIPCALL_SIGNING_KEYCHAIN:-$HOME/Library/Keychains/quipcall-signing.keychain-db}
[ -f "$KEYCHAIN" ] || deploy/mac/signing.sh "$KEYCHAIN"
security unlock-keychain -p "" "$KEYCHAIN"

VERSION=$(cargo pkgid -p quipcall-desktop | sed 's/.*@//')
BUILD=${QUIPCALL_BUILD:-$(git rev-list --count HEAD)}
case "$BUILD" in
    ''|*[!0-9]*) echo "error: QUIPCALL_BUILD must be a positive integer" >&2; exit 1 ;;
esac
[ "$BUILD" -gt 0 ] || { echo "error: QUIPCALL_BUILD must be a positive integer" >&2; exit 1; }
OUT=target/mac
rm -rf "$OUT"

for pair in aarch64-apple-darwin:apple-silicon x86_64-apple-darwin:intel; do
	target=${pair%%:*}
	name=${pair##*:}
	cargo build --release -p quipcall-desktop --target "$target"

	app=$OUT/$name/Quipcall.app
	mkdir -p "$app/Contents/MacOS"
	cp "target/$target/release/quipcall-desktop" "$app/Contents/MacOS/Quipcall"
	sed -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" -e "s/@MIN_MACOS@/$MACOSX_DEPLOYMENT_TARGET/" \
		deploy/mac/Info.plist >"$app/Contents/Info.plist"

	# Everything the app loads must ship with macOS: no @rpath / @executable_path.
	if otool -L "$app/Contents/MacOS/Quipcall" | tail -n +2 | grep -q '@'; then
		otool -L "$app/Contents/MacOS/Quipcall"
		echo "error: the $name binary links a library that isn't part of macOS" >&2
		exit 1
	fi

	# The same certificate every build, so macOS keeps the app's permissions
	# across updates. It isn't a Developer ID: Gatekeeper still needs one
	# manual "Open" for a downloaded zip.
	codesign --force --keychain "$KEYCHAIN" --sign "Quipcall Signing" "$app"
	codesign -d -r- "$app" 2>&1 | grep -q 'certificate leaf' || {
		echo "error: $app isn't signed with the Quipcall certificate" >&2
		exit 1
	}
	ditto -c -k --keepParent "$app" "$OUT/Quipcall-$name.zip"
done

echo "$VERSION ($BUILD)" >"$OUT/version.txt"
ls -lh "$OUT"/*.zip

if [ "${1:-}" = "--upload" ]; then
	# Upload next to the live files, then swap them in.
	ssh "$HOST" 'rm -rf /opt/quipcall/download/incoming && mkdir -p /opt/quipcall/download/incoming'
	scp -q "$OUT"/Quipcall-*.zip "$OUT/version.txt" "$HOST:/opt/quipcall/download/incoming/"
	ssh "$HOST" 'cd /opt/quipcall/download && chmod 644 incoming/* && mv incoming/* . && rmdir incoming'
	echo "published $VERSION (build $BUILD): https://quipcall.com/download/"
fi
