//! The macOS app menu: standard items with contextual history, guarded Quit and refresh rate.
//!
//! Our Quit item (Cmd+Q) calls `AppHandle::exit`. The run loop holds its preventable
//! `RunEvent::ExitRequested` like a window close while there are unsaved changes.
//! Dock Quit, app-switcher Quit and logout reach the same guard through the Cocoa delegate
//! method installed by `macos_quit`.
//!
//! The menu is described as data ([`spec`]) so the tests can check it without a running app;
//! [`build`] turns the description into Tauri's menu. Other platforms keep no app menu.

// Built on macOS only; the description and its tests are checked everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use tauri::menu::{
    AboutMetadata, CheckMenuItem, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu,
    WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Runtime};

/// The id of Grooviq's own Quit item.
pub(crate) const EDIT_ID: &str = "grooviq-edit";
pub(crate) const UNDO_ID: &str = "grooviq-undo";
pub(crate) const REDO_ID: &str = "grooviq-redo";
pub(crate) const QUIT_ID: &str = "grooviq-quit";
/// Its shortcut: Cmd+Q on macOS.
pub(crate) const QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";
/// The View submenu, found by this id to tick its High Refresh Rate item.
pub(crate) const VIEW_ID: &str = "grooviq-view";
/// The View > High Refresh Rate check item (see `display`).
pub(crate) const HIGH_REFRESH_ID: &str = "grooviq-high-refresh";

/// One of Tauri's predefined (native) menu items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    /// The native Quit, which skips `ExitRequested`. Never used (see the module docs); named
    /// so the tests can say it is absent.
    #[allow(dead_code)]
    Quit,
    CloseWindow,
    #[allow(dead_code)]
    Undo,
    #[allow(dead_code)]
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Fullscreen,
    Minimize,
    Maximize,
}

/// An entry of a submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Native(Native),
    Separator,
    /// Grooviq's Quit: [`QUIT_ID`], [`QUIT_ACCELERATOR`].
    Quit,
    /// The High Refresh Rate check item: [`HIGH_REFRESH_ID`], unticked until the settings load.
    HighRefresh,
    History(bool),
}

/// A submenu: its id (when macOS gives it a role), title and entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sub {
    pub id: Option<&'static str>,
    pub title: String,
    pub entries: Vec<Entry>,
}

/// The menu of an app called `app_name`: Tauri's standard macOS menu (`Menu::default`), with
/// Show All added, Grooviq's own Quit in place of the native one and High Refresh Rate in View.
pub(crate) fn spec(app_name: &str) -> Vec<Sub> {
    use Entry::{HighRefresh, Native as N, Quit, Separator};
    use Native::*;
    let sub = |id, title: &str, entries| Sub {
        id,
        title: title.to_string(),
        entries,
    };
    vec![
        sub(
            None,
            app_name,
            vec![
                N(About),
                Separator,
                N(Services),
                Separator,
                N(Hide),
                N(HideOthers),
                N(ShowAll),
                Separator,
                Quit,
            ],
        ),
        sub(None, "File", vec![N(CloseWindow)]),
        sub(
            Some(EDIT_ID),
            "Edit",
            vec![
                Entry::History(false),
                Entry::History(true),
                Separator,
                N(Cut),
                N(Copy),
                N(Paste),
                N(SelectAll),
            ],
        ),
        sub(
            Some(VIEW_ID),
            "View",
            vec![N(Fullscreen), Separator, HighRefresh],
        ),
        sub(
            Some(WINDOW_SUBMENU_ID),
            "Window",
            vec![N(Minimize), N(Maximize), Separator, N(CloseWindow)],
        ),
        sub(Some(HELP_SUBMENU_ID), "Help", vec![]),
    ]
}

/// The text of the Quit item.
pub(crate) fn quit_text(app_name: &str) -> String {
    format!("Quit {app_name}")
}

/// Whether a menu event asks to quit.
pub(crate) fn is_quit(id: &str) -> bool {
    id == QUIT_ID
}

/// Whether a menu event toggles High Refresh Rate.
pub(crate) fn is_high_refresh(id: &str) -> bool {
    id == HIGH_REFRESH_ID
}

/// Ticks or unticks View > High Refresh Rate (no menu on other platforms: nothing happens).
pub(crate) fn set_high_refresh_checked<R: Runtime>(app: &AppHandle<R>, on: bool) {
    let item = app
        .menu()
        .and_then(|menu| menu.get(VIEW_ID))
        .and_then(|view| view.as_submenu().and_then(|view| view.get(HIGH_REFRESH_ID)));
    if let Some(check) = item.as_ref().and_then(|item| item.as_check_menuitem()) {
        // Fails only while the app shuts down.
        let _ = check.set_checked(on);
    }
}

/// Builds [`spec`] as the app's menu.
pub(crate) fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let name = app.package_info().name.clone();
    let config = app.config();
    let about = AboutMetadata {
        name: Some(name.clone()),
        version: Some(app.package_info().version.to_string()),
        copyright: config.bundle.copyright.clone(),
        authors: config.bundle.publisher.clone().map(|p| vec![p]),
        ..Default::default()
    };
    let menu = Menu::new(app)?;
    for sub in spec(&name) {
        let submenu = match sub.id {
            Some(id) => Submenu::with_id(app, id, &sub.title, true)?,
            None => Submenu::new(app, &sub.title, true)?,
        };
        for entry in sub.entries {
            match entry {
                Entry::History(redo) => submenu.append(&MenuItem::with_id(
                    app,
                    if redo { REDO_ID } else { UNDO_ID },
                    if redo { "Redo" } else { "Undo" },
                    true,
                    Some(if redo {
                        "CmdOrCtrl+Shift+Z"
                    } else {
                        "CmdOrCtrl+Z"
                    }),
                )?)?,
                Entry::Separator => submenu.append(&PredefinedMenuItem::separator(app)?)?,
                Entry::Quit => submenu.append(&MenuItem::with_id(
                    app,
                    QUIT_ID,
                    quit_text(&name),
                    true,
                    Some(QUIT_ACCELERATOR),
                )?)?,
                Entry::HighRefresh => submenu.append(&CheckMenuItem::with_id(
                    app,
                    HIGH_REFRESH_ID,
                    "High Refresh Rate",
                    true,
                    false,
                    None::<&str>,
                )?)?,
                Entry::Native(native) => submenu.append(&predefined(app, native, &about)?)?,
            }
        }
        menu.append(&submenu)?;
    }
    Ok(menu)
}

fn predefined<R: Runtime>(
    app: &AppHandle<R>,
    native: Native,
    about: &AboutMetadata<'_>,
) -> tauri::Result<PredefinedMenuItem<R>> {
    match native {
        Native::About => PredefinedMenuItem::about(app, None, Some(about.clone())),
        Native::Services => PredefinedMenuItem::services(app, None),
        Native::Hide => PredefinedMenuItem::hide(app, None),
        Native::HideOthers => PredefinedMenuItem::hide_others(app, None),
        Native::ShowAll => PredefinedMenuItem::show_all(app, None),
        Native::Quit => PredefinedMenuItem::quit(app, None),
        Native::CloseWindow => PredefinedMenuItem::close_window(app, None),
        Native::Undo => PredefinedMenuItem::undo(app, None),
        Native::Redo => PredefinedMenuItem::redo(app, None),
        Native::Cut => PredefinedMenuItem::cut(app, None),
        Native::Copy => PredefinedMenuItem::copy(app, None),
        Native::Paste => PredefinedMenuItem::paste(app, None),
        Native::SelectAll => PredefinedMenuItem::select_all(app, None),
        Native::Fullscreen => PredefinedMenuItem::fullscreen(app, None),
        Native::Minimize => PredefinedMenuItem::minimize(app, None),
        Native::Maximize => PredefinedMenuItem::maximize(app, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(menu: &[Sub]) -> impl Iterator<Item = Entry> + '_ {
        menu.iter().flat_map(|s| s.entries.iter().copied())
    }

    #[test]
    fn history_events_are_unambiguous() {
        assert_eq!(history_action(UNDO_ID), Some(false));
        assert_eq!(history_action(REDO_ID), Some(true));
        assert_eq!(history_action(QUIT_ID), None);
        assert_eq!(history_action("undo"), None);
        let edit = spec("Grooviq")
            .into_iter()
            .find(|s| s.title == "Edit")
            .unwrap();
        assert_eq!(
            &edit.entries[..2],
            &[Entry::History(false), Entry::History(true)]
        );
        assert!(!edit.entries.contains(&Entry::Native(Native::Undo)));
        assert!(!edit.entries.contains(&Entry::Native(Native::Redo)));
    }

    #[test]
    fn cmd_q_is_our_own_quit_and_the_native_one_is_gone() {
        let menu = spec("Grooviq");
        assert!(
            !entries(&menu).any(|e| e == Entry::Native(Native::Quit)),
            "the native Quit ends the app without asking"
        );
        assert_eq!(entries(&menu).filter(|&e| e == Entry::Quit).count(), 1);
        // Last in the app menu, as on every Mac app.
        assert_eq!(menu[0].title, "Grooviq");
        assert_eq!(menu[0].entries.last(), Some(&Entry::Quit));
        assert_eq!(QUIT_ACCELERATOR, "CmdOrCtrl+Q");
        assert_eq!(quit_text("Grooviq"), "Quit Grooviq");
    }

    #[test]
    fn the_standard_items_stay() {
        let menu = spec("Grooviq");
        for native in [
            Native::About,
            Native::Hide,
            Native::HideOthers,
            Native::ShowAll,
        ] {
            assert!(
                menu[0].entries.contains(&Entry::Native(native)),
                "{native:?}"
            );
        }
        // Text fields keep copy and paste.
        let edit = menu.iter().find(|s| s.title == "Edit").unwrap();
        for native in [Native::Cut, Native::Copy, Native::Paste, Native::SelectAll] {
            assert!(edit.entries.contains(&Entry::Native(native)), "{native:?}");
        }
        let window = menu.iter().find(|s| s.title == "Window").unwrap();
        assert_eq!(window.id, Some(WINDOW_SUBMENU_ID));
    }

    #[test]
    fn only_the_quit_item_asks_to_exit() {
        assert!(is_quit(QUIT_ID));
        assert!(!is_quit("quit"));
        assert!(!is_quit(""));
    }
}

/// Custom items emit one history action; native responder selectors never run as well.
pub(crate) fn history_action(id: &str) -> Option<bool> {
    match id {
        UNDO_ID => Some(false),
        REDO_ID => Some(true),
        _ => None,
    }
}

#[tauri::command]
pub(crate) fn edit_history_enabled(app: AppHandle, undo: bool, redo: bool) {
    if let Some(edit) = app.menu().and_then(|menu| menu.get(EDIT_ID))
        && let Some(menu) = edit.as_submenu()
    {
        for (id, enabled) in [(UNDO_ID, undo), (REDO_ID, redo)] {
            if let Some(item) = menu.get(id)
                && let Some(item) = item.as_menuitem()
            {
                let _ = item.set_enabled(enabled);
            }
        }
    }
}
