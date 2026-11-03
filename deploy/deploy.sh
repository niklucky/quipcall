#!/bin/sh
# Deploy the server, web client and VPS config: `make deploy`.
# Builds here, uploads one bundle, runs deploy/install.sh on the box.
set -eu
cd "$(dirname "$0")/.."
HOST=${QUIPCALL_HOST:-root@testron.dev}
STAGE=/opt/quipcall/incoming

# A static Linux binary, linked by Rust's own lld: no Docker, no C toolchain.
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
	cargo build --release -p quipcall-server --target x86_64-unknown-linux-musl
(cd apps/web && pnpm install --frozen-lockfile --silent && pnpm build)

bundle=$(mktemp -d)
trap 'rm -rf "$bundle"' EXIT
cp target/x86_64-unknown-linux-musl/release/quipcall-server "$bundle/"
cp -R apps/web/dist "$bundle/web"
mkdir "$bundle/deploy"
cp -R deploy/install.sh deploy/livekit.yaml deploy/nginx deploy/systemd deploy/download "$bundle/deploy/"

# No macOS metadata: GNU tar on the box would warn about every file.
COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -C "$bundle" -czf - . |
	ssh "$HOST" "rm -rf $STAGE && mkdir -p $STAGE && tar -xzf - -C $STAGE &&
		sh $STAGE/deploy/install.sh; status=\$?; rm -rf $STAGE; exit \$status"
