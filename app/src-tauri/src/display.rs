//! High refresh rate for the web view (macOS).
//!
//! WebKit runs `requestAnimationFrame` at 60 fps even on a 120 Hz ProMotion screen, because its
//! `PreferPageRenderingUpdatesNear60FPSEnabled` feature is on by default. [`set_high_refresh`]
//! turns that feature off (or back on) for one web view through WebKit's private
//! `+[WKPreferences _features]` and `-[WKPreferences _setEnabled:forFeature:]`. Measured on
//! macOS 27.0.1, Apple M5, built-in ProMotion screen: 60 fps by default, 120 fps with the
//! feature off, on a page already running (no reload).
//!
//! Private API: every selector is checked before it is sent, so a WebKit without them leaves
//! the cap as it is and reports why. Off by default (twice the frames costs battery).

mod webkit;

use webkit::Unavailable;
#[cfg(target_os = "macos")]
use webkit::set_high_refresh;

/// Lifts or restores the cap on the main window's web view and logs the outcome. The change
/// happens on the main thread. Takes the settings and log locks: call it without either held.
pub(crate) fn apply<R: tauri::Runtime>(app: &tauri::AppHandle<R>, on: bool) {
    use tauri::Manager;

    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let app = app.clone();
    // Fails only while the window closes.
    let _ = window.with_webview(move |webview| {
        #[cfg(target_os = "macos")]
        // SAFETY: Tauri hands over its live WKWebView, on the main thread.
        let outcome = unsafe { set_high_refresh(webview.inner(), on) };
        #[cfg(not(target_os = "macos"))]
        let outcome = {
            let _ = webview;
            Err(Unavailable::NotMacOs)
        };
        report(&app, on, outcome);
    });
}

/// Logs what [`apply`] did. A failure also turns the setting off and unticks the menu item, so
/// the setting, the menu and the web view agree.
fn report<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    on: bool,
    outcome: Result<(), Unavailable>,
) {
    use tauri::Manager;

    let audio = app.try_state::<crate::Audio>();
    let (level, message) = match outcome {
        Ok(()) if on => (
            crate::audio_log::Level::Info,
            "High refresh rate on: the interface draws at the display's full rate".to_owned(),
        ),
        Ok(()) => (
            crate::audio_log::Level::Info,
            "High refresh rate off: the interface draws at up to 60 fps".to_owned(),
        ),
        Err(why) => {
            if let Some(audio) = &audio {
                crate::commands::apply_and_save(audio, || {}, |s| s.high_refresh = false);
            }
            crate::menu::set_high_refresh_checked(app, false);
            (
                crate::audio_log::Level::Warn,
                format!("High refresh rate is not available: {why}"),
            )
        }
    };
    if let Some(audio) = audio {
        crate::lock(&audio.log).push(level, message);
    }
}
