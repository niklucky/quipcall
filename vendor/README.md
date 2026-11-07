# Vendored crates

## gpui-pre-macos 0.3.11

GPUI's macOS backend, copied from crates.io with two window fixes. Search
for `quipcall patch` to locate each modification in `src/window.rs`,
`src/gpui_macos.rs`, `src/accessibility.rs`, `Cargo.toml`, and
`tests/accessibility.rs`.

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

To update: copy the new version from `~/.cargo/registry/src/*/gpui-pre-macos-<version>`,
re-apply every marked patch (including the module and test target), and bump
the `gpui-pre` pins in `apps/desktop/Cargo.toml`.
Drop this copy once upstream covers these window behaviors.

### Repeat the accessibility regression check

On macOS with Rust 1.99.1 installed, run from the repository root:

```sh
MACOSX_DEPLOYMENT_TARGET=12.3 cargo +1.99.1 test -p gpui-pre-macos --test accessibility
```

The test creates two hidden windows using the production content view.
It registers a KVO observer on `nextResponder` before or after accessibility
initialization, clears accessibility, closes the window, and removes the
observer. Both orders must complete without an exception. It also checks
children, focus, hit testing, and re-entrant queries while the accessibility
state is borrowed. No Touch Bar, microphone, camera, or screen recording is
needed.
