mod db;
mod tray;
mod worker;

use serde::Serialize;
use tauri::{Manager, State, WindowEvent};

use crate::db::Db;

/// Settings the frontend may read and write, with their allowed values. Anything else is rejected.
const SETTINGS: &[(&str, &[&str])] = &[
    ("theme", &["light", "dark", "system"]),
    ("updates.autoCheck", &["true", "false"]),
];

fn check_setting(key: &str, value: Option<&str>) -> Result<(), String> {
    let (_, allowed) = SETTINGS
        .iter()
        .find(|(k, _)| *k == key)
        .ok_or_else(|| format!("unknown setting: {key}"))?;
    match value {
        Some(v) if !allowed.contains(&v) => Err(format!("invalid value for {key}: {v}")),
        _ => Ok(()),
    }
}

#[tauri::command]
fn get_setting(db: State<'_, Db>, key: String) -> Result<Option<String>, String> {
    check_setting(&key, None)?;
    db.get_setting(&key).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_setting(db: State<'_, Db>, key: String, value: String) -> Result<(), String> {
    check_setting(&key, Some(&value))?;
    db.set_setting(&key, &value).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    schema_version: u32,
    data_dir: String,
    worker_version: Option<String>,
    /// False for dev builds (`tauri.dev.conf.json`), which must never update themselves.
    updates_enabled: bool,
}

#[tauri::command]
async fn app_info(app: tauri::AppHandle, db: State<'_, Db>) -> Result<AppInfo, String> {
    let worker_version = tauri::async_runtime::spawn_blocking(worker::ping)
        .await
        .map_err(|e| e.to_string())?
        .inspect_err(|e| log::warn!("worker ping failed: {e}"))
        .ok();
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        schema_version: db.schema_version().map_err(|e| e.to_string())?,
        data_dir: app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .display()
            .to_string(),
        worker_version,
        updates_enabled: !app.config().identifier.ends_with(".dev"),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = Db::open(&data_dir.join("earshot.db"))?;
            app.manage(db);
            tray::build(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it; the app keeps running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![get_setting, set_setting, app_info])
        .run(tauri::generate_context!())
        .expect("error while running Earshot");
}

#[cfg(test)]
mod tests {
    use super::check_setting;

    #[test]
    fn settings_are_validated() {
        assert!(check_setting("theme", Some("dark")).is_ok());
        assert!(check_setting("theme", Some("purple")).is_err());
        assert!(check_setting("updates.autoCheck", Some("false")).is_ok());
        assert!(check_setting("updates.autoCheck", Some("yes")).is_err());
        assert!(check_setting("nope", None).is_err());
        assert!(check_setting("theme", None).is_ok());
    }
}
