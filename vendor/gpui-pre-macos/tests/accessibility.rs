// quipcall patch: exercise the production content view on AppKit's main thread.
// A custom harness is needed because Rust's test threads cannot create NSViews.

#[path = "../src/accessibility.rs"]
mod accessibility;

use accessibility::AccessibleContentView;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, msg_send, runtime::AnyObject};
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, ns_string};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn tree() -> accesskit::TreeUpdate {
    let root = accesskit::NodeId(0);
    accesskit::TreeUpdate {
        nodes: vec![(root, accesskit::Node::new(accesskit::Role::Button))],
        tree: Some(accesskit::Tree::new(root)),
        tree_id: accesskit::TreeId::ROOT,
        focus: root,
    }
}

fn main() {
    let marker = MainThreadMarker::new().expect("AppKit tests must run on the main thread");
    let _app = NSApplication::sharedApplication(marker);
    for observe_before_init in [true, false] {
        check_view(marker, observe_before_init);
    }
}

fn check_view(marker: MainThreadMarker, observe_before_init: bool) {
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(160.0, 120.0));
    // SAFETY: the main thread creates the window; Retained manages its lifetime.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(marker),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let view = AccessibleContentView::new(marker, frame);
    window.setContentView(Some(&view));
    let observer = NSObject::new();
    let key = ns_string!("nextResponder");
    let observe = || unsafe {
        // Registration alone is enough to reproduce the old teardown failure.
        // The responder never changes, so this observer needs no callback.
        let _: () = msg_send![
            &*view, addObserver: &*observer, forKeyPath: key,
            options: 0usize, context: std::ptr::null_mut::<std::ffi::c_void>()
        ];
    };
    if observe_before_init {
        observe();
    }

    let activations = Arc::new(AtomicUsize::new(0));
    view.init_accessibility(gpui::A11yCallbacks {
        activation: Box::new({
            let activations = activations.clone();
            move || {
                activations.fetch_add(1, Ordering::SeqCst);
                Some(tree())
            }
        }),
        action: Box::new(|_| {}),
        deactivation: Box::new(|| {}),
    });
    if !observe_before_init {
        observe();
    }

    // AccessKit can synchronously re-enter these Objective-C methods during an
    // update. With the state borrowed, they must fall back without panicking.
    {
        let _borrow = view.ivars().borrow_mut();
        unsafe {
            let _: *mut AnyObject = msg_send![&*view, accessibilityChildren];
            let _: *mut AnyObject = msg_send![&*view, accessibilityFocusedUIElement];
            let _: *mut AnyObject =
                msg_send![&*view, accessibilityHitTest: NSPoint::new(20.0, 30.0)];
        }
        assert_eq!(activations.load(Ordering::SeqCst), 0);
    }

    // Normal queries must still activate and read the AccessKit tree.
    let children: *mut AnyObject = unsafe { msg_send![&*view, accessibilityChildren] };
    let count: usize = unsafe { msg_send![children, count] };
    assert_eq!(count, 1);
    if let Some(events) = view.update_tree(tree()) {
        events.raise();
    }
    let hit: *mut AnyObject =
        unsafe { msg_send![&*view, accessibilityHitTest: NSPoint::new(20.0, 30.0)] };
    assert!(!hit.is_null());
    if let Some(events) = view.update_focus(true) {
        events.raise();
    }
    let focus: *mut AnyObject = unsafe { msg_send![&*view, accessibilityFocusedUIElement] };
    assert!(!focus.is_null());
    assert_eq!(activations.load(Ordering::SeqCst), 1);

    view.clear();
    window.close();
    unsafe {
        let _: () = msg_send![
            &*view, removeObserver: &*observer, forKeyPath: key,
            context: std::ptr::null_mut::<std::ffi::c_void>()
        ];
    }
    println!(
        "observe_before_init={observe_before_init}: re-entrant queries, accessibility and KVO cleanup passed"
    );
}
