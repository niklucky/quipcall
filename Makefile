# Local development tasks. `make dev` runs LiveKit + the server; run `make web` / `make desktop` in other terminals.
include dev/dev.env
export

.PHONY: dev livekit server web desktop check fmt token deploy mac-app release-mac

dev:
	$(MAKE) -j2 livekit server

livekit:
	livekit-server --config dev/livekit.yaml

server:
	cargo run -p quipcall-server

web:
	cd apps/web && pnpm dev

# Opens home. `make desktop ROOM=spike` joins a room straight away.
desktop:
	cargo run -p quipcall-desktop -- $(if $(ROOM),--room $(ROOM))

check:
	cargo clippy --workspace --all-targets -- -D warnings
	cd apps/web && pnpm check

fmt:
	cargo fmt --all

# Print a join token: make token ROOM=spike ID=alice
token:
	@curl -s "http://$(QUIPCALL_BIND)/api/join?room=$(or $(ROOM),spike)&identity=$(or $(ID),cli-$$RANDOM)"; echo

# Server, web client and VPS config → https://quipcall.com (docs/DEPLOY.md).
deploy:
	deploy/deploy.sh

# The Mac app (Apple Silicon + Intel, signed, see deploy/mac/signing.sh) → target/mac/Quipcall-*.zip.
mac-app:
	deploy/mac/package.sh

# Build the Mac app and publish it at https://quipcall.com/download/.
release-mac:
	deploy/mac/package.sh --upload
