//! Global start/stop hotkey (default Ctrl+Shift+R / ⌘⇧R).

use std::str::FromStr;

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::db::Db;
use crate::recorder::Recorder;

pub const DEFAULT: &str = "CommandOrControl+Shift+R";
const SETTING: &str = "recording.hotkey";

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                let app = app.clone();
                std::thread::spawn(move || app.state::<Recorder>().toggle(&app));
            }
        })
        .build()
}

pub fn current(app: &AppHandle) -> String {
    app.state::<Db>()
        .get_setting(SETTING)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT.to_owned())
}

/// Registers the saved hotkey at startup. Failure (e.g. taken by another app) is logged only.
pub fn register_saved(app: &AppHandle) {
    let hotkey = current(app);
    match Shortcut::from_str(&hotkey) {
        Ok(s) => {
            if let Err(e) = app.global_shortcut().register(s) {
                log::warn!("could not register hotkey {hotkey}: {e}");
            }
        }
        Err(e) => log::warn!("invalid saved hotkey {hotkey}: {e}"),
    }
}

/// Replaces the hotkey. An empty string disables it.
pub fn set(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let new = if hotkey.is_empty() {
        None
    } else {
        Some(Shortcut::from_str(hotkey).map_err(|e| format!("Invalid shortcut: {e}"))?)
    };
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;
    if let Some(s) = new
        && let Err(e) = gs.register(s)
    {
        // Put the previous one back so the user is not left without a hotkey.
        register_saved(app);
        return Err(format!("That shortcut is unavailable: {e}"));
    }
    app.state::<Db>()
        .set_setting(SETTING, if hotkey.is_empty() { "" } else { hotkey })
        .map_err(|e| e.to_string())
}
