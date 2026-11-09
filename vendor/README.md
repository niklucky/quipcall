# Vendored crates

## gpui-pre-macos 0.3.11

GPUI's macOS backend, copied from crates.io with five window fixes. Search
for `quipcall patch` to locate each modification in `src/window.rs`,
`src/display_link.rs`, `src/gpui_macos.rs`, `src/accessibility.rs`,
`Cargo.toml`, and `tests/accessibility.rs`.

Title-less `PopUp` windows are created borderless. Upstream creates every
window titled, so our floating circles and toolbar had to switch the style
mask afterwards. That swaps the window's frame view and resets its first
responder. On Macs with a Touch Bar, AppKit's observer then throws while
removing a KVO observer, crashing the app
(`-[_NSTouchBarFinderObservation invalidate]`).

Joining a call closes the home window. AccessKit's subclassing adapter used
to restore the content view's Objective-C class at that point, even if
AppKit was still observing `nextResponder` on it for the Touch Bar. AppKit
then threw when removing its observer. The window now uses a fixed content
view class with AccessKit's plain adapter for accessibility queries, so
AppKit can remove its observer whenever the display cycle runs.

Floating windows and popups also set `hidesOnDeactivate` to `false` when
created. They are NSPanels, whose default hides them when another app takes
focus. That made the share picker and call controls disappear while audio
continued, especially when switching to the app being shared.

A resized window draws its new frame straight away. AppKit asks for that
frame (`displayLayer:`) during the resize, to show in the same transaction,
but GPUI throttles windows that aren't key to 30 fps, and our floating
panels never are. Whenever a frame had been drawn less than 33 ms earlier,
which is usual right after a hover, the request was dropped. The old frame
then showed stretched (or squashed) to the new size until the display link's
next frame. The request now asks for presentation, which GPUI never
throttles.

The display link also skips a drawable whose size doesn't match the layer's,
up to four in a row (`MetalFrames::skip_stale`). The link acquires drawables
ahead of time, so the first after a resize can have the old size. Drawn
into, it shows stretched until the next frame, and an idle window may not
draw again for seconds. A unit test in `src/display_link.rs` covers it; run it
with `MACOSX_DEPLOYMENT_TARGET=12.3 CARGO_TARGET_DIR=target/vendor-test cargo
test --manifest-path vendor/gpui-pre-macos/Cargo.toml --features
test-support,runtime_shaders --lib display_link`.

To update: copy the new version from `~/.cargo/registry/src/*/gpui-pre-macos-<version>`,
re-apply every marked patch (including the module and test target), and bump
the `gpui-pre` pins in `apps/desktop/Cargo.toml`.
Drop this copy once upstream covers these window behaviors.

### Repeat the accessibility regression check

On macOS with Rust 1.99.1 installed, run from the repository root:

```sh
dev/vendor-webrtc-sys.sh
MACOSX_DEPLOYMENT_TARGET=12.3 cargo +1.99.1 test -p gpui-pre-macos --test accessibility
```

The test creates two hidden windows using the production content view.
It registers a KVO observer on `nextResponder` before or after accessibility
initialization, clears accessibility, closes the window, and removes the
observer. Both orders must complete without an exception. It also checks
children, focus, hit testing, and re-entrant queries while the accessibility
state is borrowed. No Touch Bar, microphone, camera, or screen recording is
needed.

## webrtc-sys 0.3.52

Only the patch is in git: `dev/vendor-webrtc-sys.sh` (run by every `make`
target that builds) unpacks the crates.io release, checks its SHA-256, and
applies `webrtc-sys.patch`. Run it yourself before a plain `cargo` command in
a fresh checkout.

The patch changes `src/peer_connection_factory.cpp` only:

- Every WebRTC audio processor (APM) gets a capture post-processor that hands
  each 10 ms microphone frame, after echo cancellation, to a Rust callback
  (`quipcall_set_capture_processor`), then optionally runs WebRTC's adaptive
  gain control (AGC2, `quipcall_set_capture_gain_control`) on the result.
  `crates/core/src/denoise.rs` runs DeepFilterNet there, so the gain control
  follows the cleaned voice. The post-processor is the last step in the APM,
  so this is the only way to put gain control after the model. WebRTC
  processes the microphone at the lower of its rate and the codec's, so the
  APM may run at 8, 16 or 32 kHz; the callback still gets 48 kHz frames,
  resampled there and back with WebRTC's `PushResampler`.
- A capture tap (`quipcall_set_capture_tap`) gets a copy of every finished
  microphone frame, after the filter and AGC2, at the processor's rate: what
  the call sends, before encoding. `crates/core/src/audio_tap.rs` reads it
  for recordings (and, later, transcription). Offline test APMs skip it.
- Device microphone tracks take WebRTC's own noise suppression and older gain
  control (AGC1) from `quipcall_set_device_noise_suppression` and
  `quipcall_set_device_gain_control` instead of always turning them on. They
  are read when the track is created, because WebRTC re-applies the track's
  options on every unmute and renegotiation; changing the APM afterwards
  doesn't stick.
- `quipcall_test_apm_*`: an APM set up like a call's microphone on macOS, at
  any of those rates, for `crates/core/examples/denoise_eval.rs` and the tests
  in `denoise.rs`. The app doesn't call them, so the linker drops them.

To change it: edit `vendor/webrtc-sys`, then regenerate the patch against the
pristine crate (`diff -u`, paths `a/src/...` and `b/src/...`). To update the
crate: bump `VERSION` and `SHA256` (from `Cargo.lock`) in the script, re-apply
the patch, and run `python3 dev/denoise-eval.py`. Drop the patch if LiveKit's
SDK ever exposes a capture processor.
