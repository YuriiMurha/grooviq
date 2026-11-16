//! The WebKit side of [`super`]: no Tauri, so a scratch probe can compile this file alone.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// The WebKit feature that caps drawing near 60 fps.
pub(crate) const FEATURE: &str = "PreferPageRenderingUpdatesNear60FPSEnabled";

/// Why the web view's frame rate could not be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// This WebKit has no `_features` / `_setEnabled:forFeature:` (renamed or removed).
    NoPrivateApi,
    /// It has them, but no feature called [`FEATURE`].
    NoFeature,
    /// Only macOS has the cap and the switch.
    #[cfg(not(target_os = "macos"))]
    NotMacOs,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoPrivateApi => "this macOS version's web view has no setting for it",
            Self::NoFeature => "this macOS version's web view has no 60 fps cap to lift",
            #[cfg(not(target_os = "macos"))]
            Self::NotMacOs => "only the macOS web view caps drawing at 60 fps",
        })
    }
}

/// Lifts (`on`) or restores (`!on`) WebKit's 60 fps cap on `webview`.
///
/// # Safety
///
/// `webview` must point to a live `WKWebView`, and this must run on the main thread (Tauri's
/// `with_webview` closure does both).
#[cfg(target_os = "macos")]
pub(crate) unsafe fn set_high_refresh(
    webview: *mut std::ffi::c_void,
    on: bool,
) -> Result<(), Unavailable> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use std::ffi::{CStr, c_char};

    let responds = |target: *const AnyObject, sel: Sel| -> bool {
        // SAFETY: `respondsToSelector:` is public NSObject API on a live object or class.
        let yes: Bool = unsafe { msg_send![target, respondsToSelector: sel] };
        yes.as_bool()
    };
    let webview = webview.cast::<AnyObject>();
    if webview.is_null() {
        return Err(Unavailable::NoPrivateApi);
    }
    // SAFETY: public WKWebView / WKWebViewConfiguration API on the live web view (the caller's
    // contract). `preferences` is the object the web view reads, shared with its configuration.
    let prefs: *mut AnyObject = unsafe {
        let config: *mut AnyObject = msg_send![webview, configuration];
        msg_send![config, preferences]
    };
    let class = AnyClass::get(c"WKPreferences").ok_or(Unavailable::NoPrivateApi)?;
    let class_obj = (class as *const AnyClass).cast::<AnyObject>();
    if prefs.is_null()
        || !responds(class_obj, objc2::sel!(_features))
        || !responds(prefs, objc2::sel!(_setEnabled:forFeature:))
    {
        return Err(Unavailable::NoPrivateApi);
    }
    // SAFETY: both selectors were checked above. `_features` returns an NSArray of
    // `_WKFeature`, each with an NSString `key`; nothing here is retained past this call.
    unsafe {
        let features: *mut AnyObject = msg_send![class, _features];
        if features.is_null() {
            return Err(Unavailable::NoPrivateApi);
        }
        let count: usize = msg_send![features, count];
        for i in 0..count {
            let feature: *mut AnyObject = msg_send![features, objectAtIndex: i];
            if feature.is_null() || !responds(feature, objc2::sel!(key)) {
                continue;
            }
            let key: *mut AnyObject = msg_send![feature, key];
            if key.is_null() {
                continue;
            }
            let utf8: *const c_char = msg_send![key, UTF8String];
            if utf8.is_null() || CStr::from_ptr(utf8).to_bytes() != FEATURE.as_bytes() {
                continue;
            }
            // The feature caps the rate: high refresh means turning it off.
            let _: () = msg_send![prefs, _setEnabled: Bool::new(!on), forFeature: feature];
            return Ok(());
        }
    }
    Err(Unavailable::NoFeature)
}
