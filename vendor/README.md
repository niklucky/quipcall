# Vendored crates

## gpui-pre-macos 0.3.11

GPUI's macOS backend, copied from crates.io with changes in `src/window.rs` and `src/accessibility.rs`
(search for `quipcall patch`): windows of kind `PopUp` without a titlebar are
created borderless. Upstream creates every window titled, so our floating
circles and toolbar had to switch the style mask afterwards. That swaps the
window's frame view and resets its first responder, and on Macs with a Touch
Bar AppKit's Touch Bar observer then throws while removing a KVO observer,
crashing the app (`-[_NSTouchBarFinderObservation invalidate]`).

Joining a call closes the home window. AccessKit's subclassing adapter used
to restore the content view's Objective-C class at that point, even if
AppKit was still observing `nextResponder` on it for the Touch Bar. AppKit
then threw when removing its observer. The window now uses a fixed content
view class with AccessKit's plain adapter for accessibility queries, so
AppKit can remove its observer whenever the display cycle runs.

To update: copy the new version from `~/.cargo/registry/src/*/gpui-pre-macos-<version>`,
re-apply the patches, and bump the `gpui-pre` pins in `apps/desktop/Cargo.toml`.
Drop this copy once upstream covers these window behaviors.
