# Vendored crates

## gpui-pre-macos 0.3.11

GPUI's macOS backend, copied from crates.io with one change in `src/window.rs`
(search for `quipcall patch`): windows of kind `PopUp` without a titlebar are
created borderless. Upstream creates every window titled, so our floating
circles and toolbar had to switch the style mask afterwards. That swaps the
window's frame view and resets its first responder, and on Macs with a Touch
Bar AppKit's Touch Bar observer then throws while removing a KVO observer,
crashing the app (`-[_NSTouchBarFinderObservation invalidate]`).

To update: copy the new version from `~/.cargo/registry/src/*/gpui-pre-macos-<version>`,
re-apply the patch, and bump the `gpui-pre` pins in `apps/desktop/Cargo.toml`.
Drop this copy once GPUI can create borderless windows itself.

## webrtc-sys 0.3.52

Only the patch is in git: `dev/vendor-webrtc-sys.sh` (run by every `make`
target that builds) unpacks the crates.io release, checks its SHA-256, and
applies `webrtc-sys.patch`. Run it yourself before a plain `cargo` command in
a fresh checkout.

The patch changes `src/peer_connection_factory.cpp` only:

- Every WebRTC audio processor (APM) gets a capture post-processor that hands
  each 10 ms microphone frame, after echo cancellation and gain control, to a
  Rust callback (`quipcall_set_capture_processor`). `crates/core/src/denoise.rs`
  runs DeepFilterNet there.
- Device microphone tracks take WebRTC's own noise suppression from
  `quipcall_set_device_noise_suppression` instead of always turning it on. It
  is read when the track is created, because WebRTC re-applies the track's
  options on every unmute and renegotiation; changing the APM afterwards
  doesn't stick.

To change it: edit `vendor/webrtc-sys`, then regenerate the patch against the
pristine crate (`diff -u`, paths `a/src/...` and `b/src/...`). To update the
crate: bump `VERSION` and `SHA256` (from `Cargo.lock`) in the script, re-apply
the patch, and run `python3 dev/denoise-eval.py`. Drop the patch if LiveKit's
SDK ever exposes a capture processor.
