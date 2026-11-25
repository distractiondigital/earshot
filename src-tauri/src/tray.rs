//! Tray (Windows) / menu bar (macOS) icon and menu.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::recorder::Recorder;

const MAIN_WINDOW: &str = "main";

/// Menu items whose text changes with the recorder state.
struct TrayItems {
    record: MenuItem<Wry>,
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Updates the Start/Stop item to match the recorder.
pub fn set_recording(app: &AppHandle, recording: bool) {
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.record.set_text(if recording {
            "Stop Recording"
        } else {
            "Start Recording"
        });
    }
}

/// Runs `f` off the main thread (recorder start/stop joins threads and must not block the UI).
fn in_background(app: &AppHandle, f: impl FnOnce(&AppHandle) + Send + 'static) {
    let app = app.clone();
    std::thread::spawn(move || f(&app));
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let record = MenuItem::with_id(app, "record", "Start Recording", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Earshot", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Earshot", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &record,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    app.manage(TrayItems { record });

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Earshot")
        .menu(&menu)
        // Left click opens the window on Windows; the menu stays on right click.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            "record" => in_background(app, |app| app.state::<Recorder>().toggle(app)),
            "open" => show_main_window(app),
            // Exit goes through RunEvent::ExitRequested, which finalizes an active recording.
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
