# Quipcall

A small, fast app for video and audio calls, 1:1 and in groups. Native Rust desktop app (macOS first) plus a web client, running on a self-hosted LiveKit server.

- Plan and progress: [docs/PLAN.md](docs/PLAN.md)
- Architecture and decisions: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## Requirements (macOS)

- Rust (stable), Xcode
- Node 22+ and pnpm
- LiveKit server and CLI: `brew install livekit livekit-cli`

## Run locally

```sh
make dev                     # LiveKit (dev/livekit.yaml) + quipcall-server on :8080
make web                     # web client on http://127.0.0.1:5173 (first time: cd apps/web && pnpm install)
make desktop                 # desktop app, joins room "spike"
cargo run -p quipcall-desktop -- --room spike --name Alice   # options
```

Handy test participants:

```sh
# LiveKit's demo video loop
lk room join --url ws://127.0.0.1:7880 --api-key quipcall-dev \
  --api-secret quipcall-dev-secret-0123456789abcdef --identity demo-bot --publish-demo spike

# Headless Chrome with a fake camera and mic, via the web client
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new \
  --user-data-dir=/tmp/qc-chrome --use-fake-device-for-media-stream --use-fake-ui-for-media-stream \
  "http://127.0.0.1:5173/?room=spike&name=Bot&autojoin=1"
```

`QUIPCALL_NO_MIC=1` starts the desktop app without opening any audio device. Use it for headless runs.

## Development notes

- **macOS permissions.** The first time the app opens the microphone, macOS asks for permission, and WebRTC's audio thread **blocks until someone answers**. Run the app from a terminal where you can see the prompt. (A fix for this is on the Stage 1 list.)
- **Metal shaders.** GPUI compiles its shaders at runtime (`runtime_shaders` feature), so you don't need Xcode's separate Metal toolchain. Release builds should precompile them: `xcodebuild -downloadComponent MetalToolchain`, then drop the feature.
- **`-ObjC` link flag.** `apps/desktop/build.rs` adds it. libwebrtc's Objective-C categories are dropped without it.
