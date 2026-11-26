//! Bridges native Cocoa quit requests into the same guard as the Tauri run loop.

use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, available, ffi, sel};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};
use tauri::{AppHandle, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

pub(crate) fn install(app: &AppHandle) -> Result<(), &'static str> {
    let mtm =
        MainThreadMarker::new().ok_or("Cannot install the macOS quit guard off main thread")?;
    let ns_app = NSApplication::sharedApplication(mtm);
    let delegate = ns_app
        .delegate()
        .ok_or("Cannot install the macOS quit guard without an application delegate")?;
    let delegate: &AnyObject = (*delegate).as_ref();
    APP.set(app.clone())
        .map_err(|_| "The macOS quit guard was already initialized")?;
    if !add_handler(delegate.class(), application_should_terminate::<AppQuit>) {
        return Err("Keeping the application delegate's existing macOS quit method");
    }
    Ok(())
}

struct AppQuit;

impl QuitDecision for AppQuit {
    fn hold() -> bool {
        let Some((app, mtm)) = APP.get().zip(MainThreadMarker::new()) else {
            return false;
        };
        let Some(window) = app.get_webview_window("main") else {
            return false;
        };
        if !super::hold_close(app).unwrap_or(false) {
            return false;
        }

        let ns_app = NSApplication::sharedApplication(mtm);
        ns_app.unhide(None);
        let _ = window.unminimize();
        let _ = window.show();
        if available!(macos = 14.0) {
            ns_app.activate();
        } else {
            #[allow(deprecated)]
            ns_app.activateIgnoringOtherApps(true);
        }
        let _ = window.set_focus();
        true
    }
}

trait QuitDecision {
    fn hold() -> bool;
}

type QuitHandler =
    extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSApplicationTerminateReply;

extern "C-unwind" fn application_should_terminate<D: QuitDecision>(
    _: *mut AnyObject,
    _: Sel,
    _: *mut AnyObject,
) -> NSApplicationTerminateReply {
    match std::panic::catch_unwind(D::hold) {
        // Cancel even for logout. TerminateLater puts AppKit in modal-panel mode,
        // where the webview's confirmation dialog may not respond.
        Ok(true) => NSApplicationTerminateReply::TerminateCancel,
        Ok(false) => NSApplicationTerminateReply::TerminateNow,
        Err(payload) => {
            // A custom panic payload may itself panic on drop.
            std::mem::forget(payload);
            NSApplicationTerminateReply::TerminateNow
        }
    }
}

fn add_handler(class: &AnyClass, handler: QuitHandler) -> bool {
    let selector = sel!(applicationShouldTerminate:);
    // class_addMethod also allows shadowing inherited methods. Keep those, too.
    if class.instance_method(selector).is_some() {
        return false;
    }
    // SAFETY: The handler has the Cocoa ABI, encoded here as NSUInteger (Q on
    // 64-bit macOS), self (@), selector (:) and the application argument (@).
    unsafe {
        let imp = std::mem::transmute::<QuitHandler, Imp>(handler);
        ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            selector,
            imp,
            c"Q@:@".as_ptr(),
        )
        .as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::{class, msg_send, rc::Retained, runtime::ClassBuilder, sel};

    struct Decision<const HOLD: bool>;

    impl<const HOLD: bool> QuitDecision for Decision<HOLD> {
        fn hold() -> bool {
            HOLD
        }
    }

    fn reply(class: &AnyClass) -> NSApplicationTerminateReply {
        // SAFETY: The class inherits NSObject's new, and the installed quit method
        // has the Cocoa signature. The handler never reads the sender.
        unsafe {
            let delegate: Retained<AnyObject> = msg_send![class, new];
            msg_send![&*delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()]
        }
    }

    #[test]
    fn dispatches_native_quit_for_both_decisions() {
        let clean = ClassBuilder::new(c"GrooviqQuitTestClean", class!(NSObject))
            .unwrap()
            .register();
        let dirty = ClassBuilder::new(c"GrooviqQuitTestDirty", class!(NSObject))
            .unwrap()
            .register();
        assert!(add_handler(
            clean,
            application_should_terminate::<Decision<false>>
        ));
        assert!(add_handler(
            dirty,
            application_should_terminate::<Decision<true>>
        ));
        assert_eq!(reply(clean), NSApplicationTerminateReply::TerminateNow);
        assert_eq!(reply(dirty), NSApplicationTerminateReply::TerminateCancel);
    }

    #[test]
    fn preserves_an_existing_quit_method() {
        extern "C-unwind" fn original(
            _: *mut AnyObject,
            _: Sel,
            _: *mut AnyObject,
        ) -> NSApplicationTerminateReply {
            NSApplicationTerminateReply::TerminateLater
        }

        let mut builder = ClassBuilder::new(c"GrooviqQuitTestExisting", class!(NSObject)).unwrap();
        // SAFETY: The selector has one argument and the Cocoa quit signature.
        unsafe {
            builder.add_method(sel!(applicationShouldTerminate:), original as QuitHandler);
        }
        let class = builder.register();
        let subclass = ClassBuilder::new(c"GrooviqQuitTestInherited", class)
            .unwrap()
            .register();

        for class in [class, subclass] {
            let added = add_handler(class, application_should_terminate::<Decision<false>>);
            assert_eq!(reply(class), NSApplicationTerminateReply::TerminateLater);
            assert!(!added);
        }
    }

    #[test]
    fn native_quit_proceeds_if_the_decision_panics() {
        struct Panics;
        impl QuitDecision for Panics {
            fn hold() -> bool {
                panic!("quit decision failed");
            }
        }

        let class = ClassBuilder::new(c"GrooviqQuitTestPanic", class!(NSObject))
            .unwrap()
            .register();
        assert!(add_handler(class, application_should_terminate::<Panics>));
        assert_eq!(reply(class), NSApplicationTerminateReply::TerminateNow);
    }
}
