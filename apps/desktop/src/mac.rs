//! AppKit touches GPUI doesn't expose: shaped borderless windows.
//!
//! A floating circle is a square, transparent, borderless panel whose content
//! layer is clipped to a corner radius of half its size. The compositor does the
//! clipping, so video inside costs nothing extra.

use gpui::{App, Size, Window};
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindow, NSWindowSharingType, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live NSView; we're on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

fn set_radius(window: &NSWindow, radius: f64) {
    if let Some(content) = window.contentView() {
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            layer.setCornerRadius(radius);
            layer.setMasksToBounds(true);
        }
    }
}

/// Strip the window frame and shadow and clip the content to `radius`.
/// Floating windows also stay out of our own screen shares.
///
/// Deferred like [`resize`]: changing the style mask makes AppKit resize the
/// content view, and GPUI is still building the window when we're called.
pub fn make_shape(window: &Window, radius: f64, cx: &App) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    cx.foreground_executor()
        .spawn(async move {
            ns.setStyleMask(
                NSWindowStyleMask::Borderless
                    | NSWindowStyleMask::FullSizeContentView
                    | NSWindowStyleMask::NonactivatingPanel,
            );
            ns.setHasShadow(false);
            ns.setSharingType(NSWindowSharingType::None);
            set_radius(&ns, radius);
        })
        .detach();
}

/// Which point of the window stays put when it's resized.
#[derive(Clone, Copy)]
pub enum Anchor {
    Center,
    /// The toolbar grows upwards from where it sits.
    BottomCenter,
}

/// Resize to `size`, keeping `anchor` where it is, and re-clip to `radius`.
///
/// Deferred to the next run-loop turn: AppKit calls straight back into GPUI
/// when a window's frame changes, which must not happen mid-event.
pub fn resize(window: &Window, size: Size<f32>, radius: f64, anchor: Anchor, cx: &App) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    cx.foreground_executor()
        .spawn(async move {
            let frame = ns.frame();
            let (w, h) = (size.width as f64, size.height as f64);
            let x = frame.origin.x + (frame.size.width - w) / 2.0;
            // AppKit's y axis points up: origin is the bottom-left corner.
            let y = match anchor {
                Anchor::Center => frame.origin.y + (frame.size.height - h) / 2.0,
                Anchor::BottomCenter => frame.origin.y,
            };
            ns.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
            set_radius(&ns, radius);
        })
        .detach();
}
