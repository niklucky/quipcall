//! AppKit touches GPUI doesn't expose: shaped borderless windows.
//!
//! A floating circle is a square, transparent, borderless panel whose content
//! layer is masked to a circle. The compositor does the clipping, so video
//! inside costs nothing extra. GPUI can't round video corners itself.

use gpui::{App, Size, Window};
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindow, NSWindowSharingType, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGMutablePath;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CAShapeLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The visible shape of a floating window.
#[derive(Clone, Debug)]
pub enum Mask {
    /// Rounded rectangle with this corner radius.
    Rounded(f64),
    /// The union of these circles, each `(x, y, diameter)` with `(x, y)` the
    /// top-left of its bounding box in window coordinates.
    Circles(Vec<(f64, f64, f64)>),
}

impl Mask {
    /// A circle filling a `diameter`-sized window, plus an optional knob.
    pub fn circle(diameter: f32, knob: Option<(f32, f32, f32)>) -> Self {
        let mut circles = vec![(0.0, 0.0, diameter as f64)];
        circles.extend(knob.map(|(x, y, d)| (x as f64, y as f64, d as f64)));
        Mask::Circles(circles)
    }
}

/// Which point of the window stays put when it's resized.
#[derive(Clone, Copy)]
pub enum Anchor {
    Center,
    /// Resize handles: the corner opposite the handle stays put.
    TopLeft,
    /// The toolbar grows upwards from where it sits.
    BottomCenter,
}

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live NSView; we're on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

fn apply_mask(window: &NSWindow, mask: &Mask) {
    let Some(content) = window.contentView() else {
        return;
    };
    content.setWantsLayer(true);
    let Some(layer) = content.layer() else {
        return;
    };
    // No implicit animation: the shape must follow the window instantly.
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    match mask {
        Mask::Rounded(radius) => {
            // SAFETY: clearing the mask; we're on the main thread.
            unsafe { layer.setMask(None) };
            layer.setCornerRadius(*radius);
            layer.setMasksToBounds(true);
        }
        Mask::Circles(circles) => {
            let height = content.frame().size.height;
            let path = CGMutablePath::new();
            for &(x, y, d) in circles {
                // Layer coordinates have their origin at the bottom-left.
                let rect = CGRect::new(CGPoint::new(x, height - y - d), CGSize::new(d, d));
                // SAFETY: valid path, no transform.
                unsafe { CGMutablePath::add_ellipse_in_rect(Some(&path), std::ptr::null(), rect) };
            }
            let shape = CAShapeLayer::new();
            shape.setFrame(layer.bounds());
            shape.setPath(Some(&path));
            layer.setCornerRadius(0.0);
            layer.setMasksToBounds(false);
            // SAFETY: a fresh shape layer not used anywhere else.
            unsafe { layer.setMask(Some(&shape)) };
        }
    }
    CATransaction::commit();
}

/// Strip the window frame and shadow and clip the content to `mask`.
/// Floating windows also stay out of our own screen shares.
///
/// Deferred like [`resize`]: changing the style mask makes AppKit resize the
/// content view, and GPUI is still building the window when we're called.
pub fn make_shape(window: &Window, mask: Mask, cx: &App) {
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
            apply_mask(&ns, &mask);
        })
        .detach();
}

/// Change the window's visible shape (e.g. show a knob on hover).
pub fn set_mask(window: &Window, mask: Mask, cx: &App) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    // Queued behind any pending resize, so the mask matches the new size.
    cx.foreground_executor()
        .spawn(async move { apply_mask(&ns, &mask) })
        .detach();
}

/// Resize to `size`, keeping `anchor` where it is, and re-apply `mask`.
///
/// Deferred to the next run-loop turn: AppKit calls straight back into GPUI
/// when a window's frame changes, which must not happen mid-event.
pub fn resize(window: &Window, size: Size<f32>, mask: Mask, anchor: Anchor, cx: &App) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    cx.foreground_executor()
        .spawn(async move {
            let frame = ns.frame();
            let (w, h) = (size.width as f64, size.height as f64);
            // AppKit's y axis points up: origin is the bottom-left corner.
            let (x, y) = match anchor {
                Anchor::Center => (
                    frame.origin.x + (frame.size.width - w) / 2.0,
                    frame.origin.y + (frame.size.height - h) / 2.0,
                ),
                Anchor::TopLeft => (frame.origin.x, frame.origin.y + frame.size.height - h),
                Anchor::BottomCenter => (
                    frame.origin.x + (frame.size.width - w) / 2.0,
                    frame.origin.y,
                ),
            };
            ns.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
            apply_mask(&ns, &mask);
        })
        .detach();
}
