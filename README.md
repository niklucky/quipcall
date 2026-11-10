# Quipcall

Small, fast video and audio calls: a native Rust desktop app (macOS first, built with GPUI) and a Svelte web client, on a self-hosted LiveKit server.

Alpha at https://quipcall.com. Get the Mac app at https://quipcall.com/download/.

## Layout

| Path | What |
|---|---|
| `apps/desktop` | Mac app (GPUI). Home, a floating capsule of faces, a call window and a share picker |
| `apps/server` | Axum API: `/api/join` returns LiveKit tokens |
| `apps/web` | Svelte 5 web client |
| `crates/core` | Media and call logic, no UI code (LiveKit SDK, capture, video, recording) |
| `crates/stt` | Speech to text on this Mac: Silero VAD and Parakeet or Qwen3-ASR through transcribe.cpp |
| `crates/protocol` | Types shared by the server and the clients |
| `deploy/` | VPS config (nginx, systemd, LiveKit) and Mac packaging |
| `vendor/gpui-pre-macos` | GPUI's macOS backend, patched (see `vendor/README.md`) |

## Run locally

You need Rust, Xcode, Node 22+ with pnpm, and `brew install livekit livekit-cli cmake` (cmake builds transcribe.cpp, the speech-to-text engine).

```sh
make dev               # LiveKit + server on :8080
make web               # http://127.0.0.1:5173 (run `pnpm install` in apps/web first)
make desktop           # home screen; ROOM=spike joins a room straight away
make check             # clippy + svelte-check
```

To add a test participant: `lk room join --url ws://127.0.0.1:7880 --api-key quipcall-dev --api-secret quipcall-dev-secret-0123456789abcdef --identity bot --publish-demo spike`

## Ship

- `make deploy` updates the server, the web client and the VPS config.
- `make release-mac` builds the Mac apps and publishes them.
- Details are in [docs/DEPLOY.md](docs/DEPLOY.md).

## Docs

- [PLAN.md](docs/PLAN.md): stages and progress
- [ARCHITECTURE.md](docs/ARCHITECTURE.md): components and decisions
- [UI.md](docs/UI.md): the design of the call UI
- [FOOTPRINT.md](docs/FOOTPRINT.md): CPU and memory measurements

The app writes its log to `~/Library/Logs/Quipcall/quipcall.log` when launched from Finder.
