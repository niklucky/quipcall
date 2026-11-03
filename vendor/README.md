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
