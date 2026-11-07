//! AppKit touches GPUI doesn't expose: shaped borderless windows.
//!
//! A floating circle is a square, transparent, borderless panel whose content
//! layer is masked to a circle. The compositor does the clipping, so video
//! inside costs nothing extra. GPUI can't round video corners itself.

use std::ptr::NonNull;

use block2::RcBlock;
use gpui::{
    App, Size, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSApplicationDidChangeScreenParametersNotification,
    NSApplicationDidHideNotification, NSApplicationDidResignActiveNotification,
    NSApplicationDidUnhideNotification, NSView, NSWindow,
    NSWindowDidChangeOcclusionStateNotification, NSWindowDidChangeScreenNotification,
    NSWindowOcclusionState, NSWindowSharingType, NSWindowStyleMask, NSWindowWillCloseNotification,
    NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNotificationName, NSPoint, NSRect, NSSize,
};
use objc2_quartz_core::CATransaction;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The visible shape of a floating window.
#[derive(Clone, Debug)]
pub enum Mask {
    /// Rounded rectangle with this corner radius.
    Rounded(f64),
}

/// Which point of the window stays put when it's resized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Center,
    /// Resize handles: the corner opposite the handle stays put.
    TopLeft,
    /// A line across the window, `before` points from its top now and
    /// `after` points from the top once resized, stays put; so does the
    /// horizontal centre. The capsule's main row, with things growing above
    /// and below it.
    Row {
        before: f32,
        after: f32,
    },
}

/// A borderless panel above all apps and Spaces that never takes focus.
pub fn floating_options(bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(bounds),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        app_owns_titlebar_drag: true,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    }
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
    let Mask::Rounded(radius) = mask;
    // SAFETY: clearing any mask; we're on the main thread.
    unsafe { layer.setMask(None) };
    layer.setCornerRadius(*radius);
    layer.setMasksToBounds(true);
    CATransaction::commit();
}

/// Drop the shadow and clip the content to `mask`. Floating windows also stay
/// out of our own screen shares.
///
/// The window must already be borderless: GPUI creates title-less popups that
/// way (patched, see vendor/README.md). Switching the style mask here instead
/// crashed Macs with a Touch Bar.
///
/// Deferred like [`resize`]: GPUI is still building the window when we're called.
pub fn make_shape(window: &Window, mask: Mask, cx: &App) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    debug_assert!(
        !ns.styleMask().contains(NSWindowStyleMask::Titled),
        "floating windows must be created borderless"
    );
    tracing::debug!("floating window {}: {mask:?}", ns.windowNumber());
    cx.foreground_executor()
        .spawn(async move {
            ns.setHasShadow(false);
            ns.setSharingType(NSWindowSharingType::None);
            apply_mask(&ns, &mask);
        })
        .detach();
}

/// Resize to `size`, keeping `anchor` where it is, and re-apply `mask`. A
/// window that was fully on its screen stays on it, except while a resize
/// handle is dragged: moving the corner under the cursor would feed back into
/// the drag.
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
                Anchor::Row { before, after } => (
                    frame.origin.x + (frame.size.width - w) / 2.0,
                    frame.origin.y + frame.size.height - before as f64 + after as f64 - h,
                ),
            };
            let mut target = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            if !matches!(anchor, Anchor::TopLeft)
                && let Some(visible) = ns.screen().map(|s| s.visibleFrame())
                && contains(visible, frame)
            {
                target.origin.x = target.origin.x.clamp(
                    visible.origin.x,
                    (visible.origin.x + visible.size.width - w).max(visible.origin.x),
                );
                target.origin.y = target.origin.y.clamp(
                    visible.origin.y,
                    (visible.origin.y + visible.size.height - h).max(visible.origin.y),
                );
            }
            ns.setFrame_display(target, true);
            apply_mask(&ns, &mask);
        })
        .detach();
}

fn contains(outer: NSRect, inner: NSRect) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

/// Log what macOS does to our app and windows: deactivated, hidden, covered,
/// closed, moved to another display or Space. Some bugs only show on a
/// friend's Mac, and then the log is all we have.
pub fn log_window_events() {
    // SAFETY: AppKit's notification names are immutable statics.
    let events: [(&NSNotificationName, &'static str); 8] = unsafe {
        [
            (
                NSApplicationDidBecomeActiveNotification,
                "app became active",
            ),
            (
                NSApplicationDidResignActiveNotification,
                "app went to the background",
            ),
            (NSApplicationDidHideNotification, "app hidden"),
            (NSApplicationDidUnhideNotification, "app unhidden"),
            (
                NSApplicationDidChangeScreenParametersNotification,
                "displays changed",
            ),
            (
                NSWindowDidChangeOcclusionStateNotification,
                "window visibility changed",
            ),
            (NSWindowWillCloseNotification, "window closing"),
            (
                NSWindowDidChangeScreenNotification,
                "window moved to another display",
            ),
        ]
    };
    let center = NSNotificationCenter::defaultCenter();
    for (name, what) in events {
        observe(&center, name, what);
    }
    // SAFETY: as above.
    let space_changed = unsafe { NSWorkspaceActiveSpaceDidChangeNotification };
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
    observe(&workspace, space_changed, "switched Space");
}

fn observe(center: &NSNotificationCenter, name: &NSNotificationName, what: &'static str) {
    let block = RcBlock::new(move |note: NonNull<NSNotification>| {
        // SAFETY: AppKit hands us a live notification.
        let note = unsafe { note.as_ref() };
        match note.object().and_then(|o| o.downcast::<NSWindow>().ok()) {
            Some(window) => {
                let frame = window.frame();
                tracing::debug!(
                    "{what}: window {} visible={} unobscured={} level={} at ({:.0}, {:.0}) {:.0}x{:.0}",
                    window.windowNumber(),
                    window.isVisible(),
                    window
                        .occlusionState()
                        .contains(NSWindowOcclusionState::Visible),
                    window.level(),
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                );
            }
            None => tracing::info!("{what}"),
        }
    });
    // SAFETY: the block only logs. With no queue it runs on the posting
    // thread, the main thread for these. The center keeps the registration
    // alive; the returned token is only needed to unregister.
    let _ = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
    };
}
