//! macOS menu bar + app-wide shortcuts (Cmd-W, Cmd-Q, Cmd-H, Cmd-M, Cmd-,).
//!
//! gpui installs no menu bar and no default key bindings, so without this none
//! of the standard window/app shortcuts do anything.

use crate::controller::Controller;
use gpui::{App, AsyncApp, Entity, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

actions!(stenly, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom, CloseWindow, OpenSettings, CheckUpdates, FindMeetings]);

pub fn init(controller: Entity<Controller>, cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-f", FindMeetings, None),
        KeyBinding::new("ctrl-f", FindMeetings, None),
        KeyBinding::new("cmd-k", FindMeetings, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);

    {
        let ctrl = controller.clone();
        cx.on_action(move |_: &Quit, cx| {
            ctrl.update(cx, |c, cx| c.stop_recording(cx));
            let ctrl = ctrl.clone();
            cx.spawn(async move |cx: &mut AsyncApp| crate::safe_quit(ctrl, cx).await).detach();
        });
    }
    let update_ctrl = controller.clone();
    cx.on_action(move |_: &CheckUpdates, cx| crate::updates::check(update_ctrl.clone(), cx));
    cx.on_action(|_: &FindMeetings, cx| {
        let Some(w) = cx.active_window().and_then(|w| w.downcast::<crate::main_window::MainWindow>()) else { return };
        cx.defer(move |cx| {
            let _ = w.update(cx, |this, window, cx| {
                this.open_search(window, cx);
            });
        });
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| on_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| on_active_window(cx, |window| window.zoom_window()));
    // Closing the window keeps Stenly running in the menubar (QuitMode::Explicit).
    cx.on_action(|_: &CloseWindow, cx| on_active_window(cx, |window| window.remove_window()));
    {
        let ctrl = controller.clone();
        cx.on_action(move |_: &OpenSettings, cx| {
            crate::main_window::open(ctrl.clone(), Some(crate::main_window::Page::Preferences), cx);
        });
    }

    use crate::text_input;
    cx.set_menus([
        Menu::new("Stenly").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Check for Updates…", CheckUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Stenly", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Stenly", Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Find Meetings…", FindMeetings),
            MenuItem::separator(),
            MenuItem::os_action("Cut", text_input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", text_input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", text_input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", text_input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]);
}

/// Run `f` on the key window. Deferred: a keyboard shortcut dispatches while
/// gpui is still updating that window, so updating it synchronously from the
/// action handler fails ("window not found") and the shortcut silently does nothing.
fn on_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window) + 'static) {
    let Some(w) = cx.active_window() else { return };
    cx.defer(move |cx| {
        if let Err(e) = w.update(cx, |_, window, _| f(window)) {
            tracing::warn!(error = %e, "window shortcut failed");
        }
    });
}
