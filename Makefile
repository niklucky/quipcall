# Local development tasks. `make dev` runs LiveKit + the server; run `make web` / `make desktop` in other terminals.
include dev/dev.env
export

.PHONY: dev livekit server web desktop check fmt deploy mac-app release-mac vendor

# Cargo needs vendor/webrtc-sys (crates.io source + our patch) before any build.
vendor:
	@dev/vendor-webrtc-sys.sh

dev:
	$(MAKE) -j2 livekit server

livekit:
	livekit-server --config dev/livekit.yaml

server: vendor
	cargo run -p quipcall-server

web:
	cd apps/web && pnpm dev

# Opens home. `make desktop ROOM=spike` joins a room straight away.
desktop: vendor
	cargo run -p quipcall-desktop -- $(if $(ROOM),--room $(ROOM))

check: vendor
	cargo clippy --workspace --all-targets -- -D warnings
	cd apps/web && pnpm check

fmt: vendor
	cargo fmt --all

# The former unauthenticated GET /api/join is intentionally gone.

# Server, web client and VPS config → https://quipcall.com (docs/DEPLOY.md).
deploy: vendor
	deploy/deploy.sh

# The Mac app (Apple Silicon + Intel, signed, see deploy/mac/signing.sh) → target/mac/Quipcall-*.zip.
mac-app: vendor
	deploy/mac/package.sh

# Build the Mac app and publish it at https://quipcall.com/download/.
release-mac: vendor
	deploy/mac/package.sh --upload
