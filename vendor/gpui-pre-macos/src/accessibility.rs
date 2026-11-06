//! quipcall patch: keep the window content view's Objective-C class stable.
//! AppKit observes nextResponder on this view when the Touch Bar is active.
//! Restoring its class on close would discard AppKit's observer registration.

use accesskit_macos::{Adapter, QueuedEvents};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::AnyObject,
};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect};
use std::cell::RefCell;

pub(crate) struct Accessibility {
    adapter: Adapter,
    activate: Activation,
}

struct Activation(Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send>);
impl accesskit::ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.0)()
    }
}

struct Action(Box<dyn Fn(accesskit::ActionRequest) + Send>);
impl accesskit::ActionHandler for Action {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[name = "GPUIAccessibleContentView"]
    #[ivars = RefCell<Option<Accessibility>>]
    pub(crate) struct AccessibleContentView;

    impl AccessibleContentView {
        #[unsafe(method(accessibilityChildren))]
        fn children(&self) -> *mut AnyObject {
            if let Some(ax) = self.ivars().borrow_mut().as_mut() {
                return ax.adapter.view_children(&mut ax.activate).cast();
            }
            unsafe { msg_send![super(self), accessibilityChildren] }
        }

        #[unsafe(method(accessibilityFocusedUIElement))]
        fn focus(&self) -> *mut AnyObject {
            if let Some(ax) = self.ivars().borrow_mut().as_mut() {
                return ax.adapter.focus(&mut ax.activate).cast();
            }
            unsafe { msg_send![super(self), accessibilityFocusedUIElement] }
        }

        #[unsafe(method(accessibilityHitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut AnyObject {
            if self.window().is_some()
                && let Some(ax) = self.ivars().borrow_mut().as_mut()
            {
                let point = accesskit_macos::NSPoint::new(point.x, point.y);
                return ax.adapter.hit_test(point, &mut ax.activate).cast();
            }
            unsafe { msg_send![super(self), accessibilityHitTest: point] }
        }
    }
);

impl AccessibleContentView {
    pub(crate) fn new(marker: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(marker).set_ivars(RefCell::new(None));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub(crate) fn init_accessibility(&self, callbacks: gpui::A11yCallbacks) {
        let focused = self.window().is_some_and(|w| w.isKeyWindow());
        // SAFETY: this is a live NSView on the main thread. Adapter holds it weakly.
        let adapter = unsafe {
            Adapter::new(
                self as *const Self as *mut _,
                focused,
                Action(callbacks.action),
            )
        };
        self.ivars().replace(Some(Accessibility {
            adapter,
            activate: Activation(callbacks.activation),
        }));
    }

    pub(crate) fn update_tree(&self, tree: accesskit::TreeUpdate) -> Option<QueuedEvents> {
        self.ivars()
            .borrow_mut()
            .as_mut()?
            .adapter
            .update_if_active(|| tree)
    }

    pub(crate) fn update_focus(&self, focused: bool) -> Option<QueuedEvents> {
        self.ivars()
            .borrow_mut()
            .as_mut()?
            .adapter
            .update_view_focus_state(focused)
    }

    pub(crate) fn clear(&self) {
        self.ivars().replace(None);
    }
}
