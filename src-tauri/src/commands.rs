//! Tauri commands exposed to the webview. Everything that blocks (device I/O, joining threads)
//! runs on a blocking worker thread, never on the main thread.

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::db::{Db, MeetingRow};
use crate::recorder::capture::{self, AudioApp, InputDevice};
use crate::recorder::{self, Recorder, RecorderState, StartOptions};
use crate::{hotkey, worker};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

// ---- Settings ----

/// Settings the frontend may write, with allowed values (`None` = any short string).
/// The recordings folder and hotkey have dedicated commands that validate them.
const SETTINGS: &[(&str, Option<&[&str]>)] = &[
    ("theme", Some(&["light", "dark", "system"])),
    ("updates.autoCheck", Some(&["true", "false"])),
    ("recording.micDevice", None),
    // "all", "none" or a JSON SystemSource; validated in check_setting.
    ("recording.systemSource", None),
    ("recording.showPill", Some(&["true", "false"])),
    ("recording.consentReminder", Some(&["true", "false"])),
    ("ui.headphonesTipShown", Some(&["true", "false"])),
];
/// Readable but only changed through dedicated commands.
const READ_ONLY_SETTINGS: &[&str] = &["recording.dir", "recording.hotkey"];

pub(crate) fn check_setting(key: &str, value: Option<&str>) -> Result<(), String> {
    if value.is_none() && READ_ONLY_SETTINGS.contains(&key) {
        return Ok(());
    }
    let (_, allowed) = SETTINGS
        .iter()
        .find(|(k, _)| *k == key)
        .ok_or_else(|| format!("unknown setting: {key}"))?;
    if key == "recording.systemSource"
        && let Some(v) = value
        && !matches!(v, "all" | "none")
        && serde_json::from_str::<capture::SystemSource>(v).is_err()
    {
        return Err(format!("invalid meeting audio source: {v}"));
    }
    match (value, allowed) {
        (Some(v), Some(allowed)) if !allowed.contains(&v) => {
            Err(format!("invalid value for {key}: {v}"))
        }
        (Some(v), None) if v.len() > 512 => Err(format!("value too long for {key}")),
        _ => Ok(()),
    }
}

#[tauri::command]
pub fn get_setting(db: State<'_, Db>, key: String) -> Result<Option<String>, String> {
    check_setting(&key, None)?;
    db.get_setting(&key).map_err(err)
}

#[tauri::command]
pub fn set_setting(db: State<'_, Db>, key: String, value: String) -> Result<(), String> {
    check_setting(&key, Some(&value))?;
    db.set_setting(&key, &value).map_err(err)
}

// ---- App info ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    schema_version: u32,
    data_dir: String,
    worker_version: Option<String>,
    /// False for dev builds (`tauri.dev.conf.json`), which must never update themselves.
    updates_enabled: bool,
}

#[tauri::command]
pub async fn app_info(app: AppHandle, db: State<'_, Db>) -> Result<AppInfo, String> {
    let worker_version = tauri::async_runtime::spawn_blocking(worker::ping)
        .await
        .map_err(err)?
        .inspect_err(|e| log::warn!("worker ping failed: {e}"))
        .ok();
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        schema_version: db.schema_version().map_err(err)?,
        data_dir: app
            .path()
            .app_data_dir()
            .map_err(err)?
            .display()
            .to_string(),
        worker_version,
        updates_enabled: !app.config().identifier.ends_with(".dev"),
    })
}

// ---- Recorder ----

#[tauri::command]
pub fn recorder_state(recorder: State<'_, Recorder>) -> RecorderState {
    recorder.state()
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    options: StartOptions,
) -> Result<RecorderState, String> {
    blocking(move || app.state::<Recorder>().start(&app, options)).await
}

#[tauri::command]
pub async fn resume_recording(app: AppHandle, id: i64) -> Result<RecorderState, String> {
    blocking(move || app.state::<Recorder>().resume(&app, id)).await
}

#[tauri::command]
pub fn interrupted_meeting(db: State<'_, Db>) -> Option<recorder::Interrupted> {
    recorder::interrupted(&db)
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> Result<i64, String> {
    blocking(move || app.state::<Recorder>().stop(&app)).await
}

#[tauri::command]
pub fn pause_recording(
    app: AppHandle,
    recorder: State<'_, Recorder>,
    paused: bool,
) -> Result<RecorderState, String> {
    recorder.set_paused(&app, paused)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sources {
    input_devices: Vec<InputDevice>,
    output_devices: Vec<InputDevice>,
    apps: Vec<AudioApp>,
    supports_app_capture: bool,
}

#[tauri::command]
pub async fn list_sources() -> Result<Sources, String> {
    blocking(|| {
        let supports_app_capture = capture::supports_app_capture();
        Ok(Sources {
            input_devices: capture::list_input_devices(),
            output_devices: capture::list_output_devices(),
            apps: if supports_app_capture {
                capture::list_audio_apps()
            } else {
                Vec::new()
            },
            supports_app_capture,
        })
    })
    .await
}

// ---- Meetings ----

#[tauri::command]
pub fn list_meetings(db: State<'_, Db>) -> Result<Vec<MeetingRow>, String> {
    db.list_meetings().map_err(err)
}

#[tauri::command]
pub fn update_meeting(
    db: State<'_, Db>,
    id: i64,
    title: String,
    speaker_hint: Option<i64>,
    tags: Vec<String>,
) -> Result<(), String> {
    let title: String = title.trim().chars().take(200).collect();
    let tags: Vec<String> = tags
        .iter()
        .map(|t| t.trim().chars().take(40).collect::<String>())
        .filter(|t| !t.is_empty())
        .take(20)
        .collect();
    if speaker_hint.is_some_and(|n| !(1..=20).contains(&n)) {
        return Err("Speaker count must be between 1 and 20".into());
    }
    db.update_meeting_details(id, &title, speaker_hint, &tags)
        .map_err(err)
}

#[tauri::command]
pub async fn recover_meeting(app: AppHandle, id: i64) -> Result<i64, String> {
    blocking(move || recorder::recover(&app.state::<Db>(), id)).await
}

// ---- Storage & hotkey ----

#[tauri::command]
pub fn recordings_dir(app: AppHandle) -> Result<String, String> {
    Ok(recorder::recordings_dir(&app)?.display().to_string())
}

/// Opens a folder picker and saves the choice. Returns the new folder, or `None` if cancelled.
#[tauri::command]
pub async fn choose_recordings_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let Some(picked) = app
            .dialog()
            .file()
            .set_title("Choose recordings folder")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(err)?;
        // Make sure it is writable before saving it.
        let probe = path.join(".earshot-write-test");
        std::fs::write(&probe, b"")
            .map_err(|e| format!("Cannot write to {}: {e}", path.display()))?;
        let _ = std::fs::remove_file(probe);
        let path = path.display().to_string();
        app.state::<Db>()
            .set_setting("recording.dir", &path)
            .map_err(err)?;
        Ok(Some(path))
    })
    .await
}

#[tauri::command]
pub fn open_recordings_dir(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = recorder::recordings_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(err)?;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(err)
}

#[tauri::command]
pub fn get_hotkey(app: AppHandle) -> String {
    hotkey::current(&app)
}

#[tauri::command]
pub fn set_hotkey(app: AppHandle, hotkey: String) -> Result<(), String> {
    hotkey::set(&app, hotkey.trim())
}

#[cfg(test)]
mod tests {
    use super::check_setting;

    #[test]
    fn settings_are_validated() {
        assert!(check_setting("theme", Some("dark")).is_ok());
        assert!(check_setting("theme", Some("purple")).is_err());
        assert!(check_setting("updates.autoCheck", Some("yes")).is_err());
        assert!(check_setting("recording.micDevice", Some("{0.0.1.00000000}.{abc}")).is_ok());
        assert!(check_setting("recording.micDevice", Some(&"x".repeat(600))).is_err());
        assert!(check_setting("recording.dir", None).is_ok());
        assert!(check_setting("recording.dir", Some("C:\\anywhere")).is_err());
        assert!(check_setting("nope", None).is_err());
        assert!(check_setting("recording.systemSource", Some("all")).is_ok());
        assert!(
            check_setting(
                "recording.systemSource",
                Some(r#"{"kind":"output","id":"{0.0.0}.{x}","name":"Voice Chat"}"#)
            )
            .is_ok()
        );
        assert!(check_setting("recording.systemSource", Some("bogus")).is_err());
    }
}
