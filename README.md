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

Web client URL options for test participants: `autojoin=1`, `codec=vp8|vp9|av1|h264` (camera codec, default `h264`), `mic=0` (no microphone, so no beeping fake audio). In dev builds the LiveKit room is available as `window.qcRoom`, so a script can read WebRTC stats over the DevTools protocol (`--remote-debugging-port`).

`QUIPCALL_NO_MIC=1` starts the desktop app without opening any audio device. Use it for headless runs.

To run a second, separate stack next to `make dev` (for example for automated tests), use other ports: LiveKit with a copy of `dev/livekit.yaml`, the server with `QUIPCALL_BIND` and `LIVEKIT_PUBLIC_URL`, Vite with `QUIPCALL_API=http://127.0.0.1:<server port> vite --port <port>`, and the desktop app with `--server`.

## Development notes

- **macOS permissions.** The app asks for microphone access before joining and for camera access when you turn the camera on, and waits for your answer ("Waiting for … access" in the header). We ask up front because a prompt that WebRTC triggers itself blocks its audio thread and freezes the call. When run from a terminal, macOS attributes the request to the terminal app (for example "Terminal would like to access the camera").
- **Metal shaders.** GPUI compiles its shaders at runtime (`runtime_shaders` feature), so you don't need Xcode's separate Metal toolchain. Release builds should precompile them: `xcodebuild -downloadComponent MetalToolchain`, then drop the feature.
- **`-ObjC` link flag.** `apps/desktop/build.rs` adds it. libwebrtc's Objective-C categories are dropped without it.
