# Quipcall

A small, fast app for video and audio calls, 1:1 and in groups. Native Rust desktop app (macOS first) plus a web client, running on a self-hosted LiveKit server.

- Plan and progress: [docs/PLAN.md](docs/PLAN.md)
- Architecture and decisions: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Deploying (https://quipcall.com) and the Mac app: [docs/DEPLOY.md](docs/DEPLOY.md) — `make deploy`, `make release-mac`

## Requirements (macOS)

- Rust (stable), Xcode
- Node 22+ and pnpm
- LiveKit server and CLI: `brew install livekit livekit-cli`

## Run locally

```sh
make dev                     # LiveKit (dev/livekit.yaml) + quipcall-server on :8080
make web                     # web client on http://127.0.0.1:5173 (first time: cd apps/web && pnpm install)
make desktop                 # desktop app: home screen with rooms and history
make desktop ROOM=spike      # join a room straight away
cargo run -p quipcall-desktop -- --room spike --name Alice --regular   # options
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

Desktop app options for tests:
- `QUIPCALL_NO_MIC=1` starts the app without opening any audio device.
- `QUIPCALL_FAKE_CAMERA=1` replaces the camera with a synthetic 720p30 pattern that goes through the real encode path (no camera, no permission prompt).
- `--camera` turns the camera on as soon as the call connects; `--regular` starts calls in the tile window instead of circles.
- `QUIPCALL_DATA_DIR=/tmp/x` keeps rooms and history somewhere other than `~/Library/Application Support/Quipcall`.
- `cargo run -p quipcall-desktop --features snapshot` with `QUIPCALL_SNAPSHOT_DIR=/tmp/snaps` writes a PNG of every window every 2 s, to check layouts without screen-recording access (AppKit's circle clip and blur aren't in the image). `QUIPCALL_SCRIPT="4:regular 8:minimal 12:leave"` presses buttons on a timer (see `src/snapshot.rs`).

Measuring: `dev/measure.sh $(pgrep -x quipcall-desktop) 8` samples CPU and the physical footprint (use the footprint, not RSS). For a 10-person call, `lk load-test --room <room> --video-publishers 9 --video-codec h264` adds nine simulated publishers.

To run a second, separate stack next to `make dev` (for example for automated tests), use other ports: LiveKit with a copy of `dev/livekit.yaml`, the server with `QUIPCALL_BIND` and `LIVEKIT_PUBLIC_URL`, Vite with `QUIPCALL_API=http://127.0.0.1:<server port> vite --port <port>`, and the desktop app with `--server`.

## Development notes

- **macOS permissions.** The app asks for microphone access before joining and for camera access when you turn the camera on, and waits for your answer ("Waiting for … access" in the header). We ask up front because a prompt that WebRTC triggers itself blocks its audio thread and freezes the call. When run from a terminal, macOS attributes the request to the terminal app (for example "Terminal would like to access the camera").
- **Metal shaders.** GPUI compiles its shaders at runtime (`runtime_shaders` feature), so you don't need Xcode's separate Metal toolchain. Release builds should precompile them: `xcodebuild -downloadComponent MetalToolchain`, then drop the feature.
- **Patched GPUI.** `vendor/gpui-pre-macos` is GPUI's macOS backend with one change, so floating windows are created borderless instead of switching later (that crashed Macs with a Touch Bar). See `vendor/README.md`.
- **Logs.** Started from Finder, the app writes `~/Library/Logs/Quipcall/quipcall.log`, including what macOS does to its windows (hidden, covered, moved, app activated or not), for bugs we can only see on other people's Macs.
- **`-ObjC` link flag.** `apps/desktop/build.rs` adds it. libwebrtc's Objective-C categories are dropped without it.
