# Vendored crates

## gpui-pre-macos 0.3.11

GPUI's macOS backend, copied from crates.io with changes in `src/window.rs`
(search for `quipcall patch`): windows of kind `PopUp` without a titlebar are
created borderless. Upstream creates every window titled, so our floating
circles and toolbar had to switch the style mask afterwards. That swaps the
window's frame view and resets its first responder, and on Macs with a Touch
Bar AppKit's Touch Bar observer then throws while removing a KVO observer,
crashing the app (`-[_NSTouchBarFinderObservation invalidate]`).

Floating windows and popups also set `hidesOnDeactivate` to `false` when
created. They are NSPanels, whose default hides them when another app takes
focus. That made the share picker and call controls disappear while audio
continued, especially when switching to the app being shared.

To update: copy the new version from `~/.cargo/registry/src/*/gpui-pre-macos-<version>`,
re-apply the patches, and bump the `gpui-pre` pins in `apps/desktop/Cargo.toml`.
Drop this copy once upstream covers these window behaviors.
