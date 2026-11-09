# Quipcall: notes for agents

Read README.md first. Plan: docs/PLAN.md. Decisions: docs/ARCHITECTURE.md.

## Priorities
- **Footprint first.** Keep binaries small and CPU and memory low. Don't add a dependency without a reason.
- **UI is minimal and custom.** It's built on GPUI with no widget library. Less on screen is better (docs/UI.md).
- `crates/core` contains no UI code. Media logic goes there.

## Conventions
- In PLAN.md, finished tasks are marked `[x] ~~task~~` and finished stages `## ~~Stage N~~ ✅`. Update it as work lands.
- Run `make check` (clippy with `-D warnings`, plus svelte-check) and `cargo fmt --all` before committing.
- Cargo needs `vendor/webrtc-sys` (crates.io source + `vendor/webrtc-sys.patch`, gitignored). `make` targets create it; in a fresh checkout run `dev/vendor-webrtc-sys.sh` before plain `cargo`.
- Commit locally only. Push only when asked.

## Never
- Kill the user's `make dev` or `make web` (ports 7880, 8080 and 5173). Run your own stack on other ports instead: LiveKit 7980, server 8180 (`QUIPCALL_BIND`), Vite 5273 (`QUIPCALL_API`). Point the app at it with `--server`.
- Capture the user's screen. T3 Code has Screen Recording permission, so ScreenCaptureKit sees real windows. Don't open the share picker and don't share a display or another app's window. Use the `share-own` script action and read only numbers and logs.
- Turn on the real camera without asking. Use `QUIPCALL_FAKE_CAMERA=1 --camera` instead.
- Touch other projects on the VPS (`root@testron.dev`), which is shared. Run `nginx -t` before every reload.

## Testing the desktop app
- `QUIPCALL_DATA_DIR=/tmp/qc-data` keeps the user's rooms and history out of tests.
- `QUIPCALL_NO_MIC=1` starts the app without opening an audio device.
- `--features snapshot` with `QUIPCALL_SNAPSHOT_DIR` saves a PNG of every window. The AppKit capsule clip and blur don't show in it, and a window hidden behind others stops drawing, so its PNG keeps an old frame. `QUIPCALL_SCRIPT` actions `hover`, `unhover`, `stack`, `panel` and `people` (and `QUIPCALL_PANEL=transcript|people`) reach the new UI states.
- `QUIPCALL_SCRIPT="4:regular 8:leave"` presses buttons on a timer (`src/snapshot.rs`).
- `QUIPCALL_FAKE_SCREEN=1` (or `=1600x900`) with the `share-own` script action shares a test pattern instead of anything on screen. Two app instances can share at once this way.
- Without a terminal the app logs to `~/Library/Logs/Quipcall/`, which is the user's log. Run test instances under `script -q <logfile> <app> …` to keep them out of it.
- Headless web participant: Chrome with `--headless=new --use-fake-device-for-media-stream --use-fake-ui-for-media-stream`, opening `?room=X&autojoin=1&mic=0`. The room is `window.qcRoom`, and its stats can be read over the DevTools protocol.
- Noise suppression: `QUIPCALL_DENOISE=webrtc` (WebRTC's filter) or `=off` instead of DeepFilterNet, and `QUIPCALL_DENOISE_LIMIT=15/30` (dB while talking / in pauses, or `none`) for a stronger model, for A/B. `python3 dev/denoise-eval.py` runs real voices and noise through a call's audio processing and scores every chain; listen in `target/denoise-eval/out/`.
- Measure memory with `dev/measure.sh <pid>`, which reports the physical footprint, not RSS. RSS once hid a 3 GB IOSurface leak.

## macOS gotchas
- Ask for mic and camera access with AVCaptureDevice before WebRTC touches them. A prompt raised by WebRTC itself blocks its audio thread and freezes the call.
- Any thread that handles libwebrtc Objective-C frames needs an autorelease pool per frame (`autorelease_pool`).
- Never change a window's style mask at runtime: it crashes Macs with a Touch Bar. Floating windows are created borderless through the patched GPUI in `vendor/`.
- Never change an NSView's class at runtime (`object_setClass`, AccessKit's `SubclassingAdapter`). AppKit's Touch Bar observes the responder chain with KVO and can throw when removing its observer after a class change. Use a fixed view class and AccessKit's plain adapter.
- Defer AppKit frame changes with a `foreground_executor` spawn. Defer opening a window from inside an entity update with `cx.defer`, or GPUI panics.
- Keep the camera on H.264 with zero-copy rendering: it uses about 40% less CPU than VP8.
- Mac builds need `MACOSX_DEPLOYMENT_TARGET=12.3`. Below that, the Swift libraries link via `@rpath` and the app won't start. Dev builds are ad-hoc signed, so macOS asks for permissions again after every build. Releases are signed with the certificate in `~/Library/Keychains/quipcall-signing.keychain-db` so they keep them; never delete or regenerate it.
- GPUI compiles its shaders at runtime (the `runtime_shaders` feature), so no Metal toolchain is needed. `apps/desktop/build.rs` adds `-ObjC`; without it, libwebrtc's Objective-C categories are dropped.
- This Mac's network runs a fake-IP proxy, so local `dig` gives wrong answers. Resolve names from the VPS instead.
