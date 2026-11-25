mod db;
mod tray;
mod worker;

use serde::Serialize;
use tauri::{Manager, State, WindowEvent};

use crate::db::Db;

/// Settings keys the frontend may read and write. Anything else is rejected.
const SETTINGS_KEYS: &[&str] = &["theme"];

fn check_key(key: &str) -> Result<(), String> {
    if SETTINGS_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(format!("unknown setting: {key}"))
    }
}

#[tauri::command]
fn get_setting(db: State<'_, Db>, key: String) -> Result<Option<String>, String> {
    check_key(&key)?;
    db.get_setting(&key).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_setting(db: State<'_, Db>, key: String, value: String) -> Result<(), String> {
    check_key(&key)?;
    if key == "theme" && !matches!(value.as_str(), "light" | "dark" | "system") {
        return Err(format!("invalid theme: {value}"));
    }
    db.set_setting(&key, &value).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    schema_version: u32,
    data_dir: String,
    worker_version: Option<String>,
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
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
