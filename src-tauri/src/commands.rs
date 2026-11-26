//! Tauri commands exposed to the webview. Everything that blocks (device I/O, joining threads)
//! runs on a blocking worker thread, never on the main thread.

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::db::{Db, MeetingRow};
use crate::recorder::capture::{self, AudioApp, InputDevice};
use crate::recorder::{self, Recorder, RecorderState, StartOptions};
use crate::{hotkey, models, worker};
use tauri::Emitter;

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
    // "auto" or an ISO 639-1 code.
    ("transcription.language", None),
    ("transcription.backend", Some(&["auto", "gpu", "cpu"])),
    ("transcription.autoProcess", Some(&["true", "false"])),
    // 0–100; validated in check_setting.
    ("speakers.sensitivity", None),
    // Name shown for the mic track's speaker.
    ("speakers.meName", None),
    // Recognise saved voices in new meetings.
    ("speakers.autoMatch", Some(&["true", "false"])),
    // Similarity needed to recognise a voice (0.3–0.95); validated in check_setting.
    ("speakers.matchThreshold", None),
    // Summaries: never, on demand, or automatically after processing.
    ("summary.mode", Some(&["never", "manual", "auto"])),
    // Title untitled meetings with the summary model.
    ("summary.autoTitle", Some(&["true", "false"])),
    // Default template id; validated in check_setting.
    ("summary.template", None),
];
/// Readable but only changed through dedicated commands.
const READ_ONLY_SETTINGS: &[&str] = &[
    "recording.dir",
    "recording.hotkey",
    "transcription.model",
    "speakers.embeddingModel",
    "summary.model",
];

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
    if key == "speakers.sensitivity"
        && let Some(v) = value
        && !v.parse::<f32>().is_ok_and(|n| (0.0..=100.0).contains(&n))
    {
        return Err(format!("sensitivity must be 0–100: {v}"));
    }
    if key == "speakers.matchThreshold"
        && let Some(v) = value
        && !v.parse::<f32>().is_ok_and(|n| (0.3..=0.95).contains(&n))
    {
        return Err(format!("match threshold must be 0.3–0.95: {v}"));
    }
    if key == "summary.template"
        && let Some(v) = value
        && v.parse::<i64>().is_err()
    {
        return Err(format!("invalid template: {v}"));
    }
    if key == "speakers.meName"
        && let Some(v) = value
        && (v.trim().is_empty() || v.chars().count() > 60)
    {
        return Err("the name must be 1–60 characters".into());
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
    let worker_version = tauri::async_runtime::spawn_blocking(worker::info)
        .await
        .map_err(err)?
        .map(|w| match w.gpu_backend {
            Some(gpu) => format!("{} ({gpu})", w.version),
            None => w.version,
        });
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
    blocking(move || {
        let ms = recorder::recover(&app.state::<Db>(), id)?;
        crate::processing::auto_enqueue(&app, id);
        Ok(ms)
    })
    .await
}

// ---- Models ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsInfo {
    hardware: models::hardware::Hardware,
    recommended: &'static str,
    recommended_summary: &'static str,
    models: Vec<models::ModelView>,
}

fn hardware() -> &'static models::hardware::Hardware {
    static HW: std::sync::OnceLock<models::hardware::Hardware> = std::sync::OnceLock::new();
    HW.get_or_init(|| models::hardware::detect(worker::info().and_then(|w| w.gpu_backend)))
}

#[tauri::command]
pub async fn list_models(app: AppHandle) -> Result<ModelsInfo, String> {
    blocking(move || {
        let hw = hardware().clone();
        let recommended = models::hardware::recommend(&hw);
        let recommended_summary = models::hardware::recommend_summary(&hw);
        Ok(ModelsInfo {
            models: models::list(&app, &[recommended, recommended_summary]),
            hardware: hw,
            recommended,
            recommended_summary,
        })
    })
    .await
}

/// Downloads a model (and the small VAD model alongside a transcription model).
#[tauri::command]
pub fn download_model(app: AppHandle, id: String) -> Result<(), String> {
    let m = models::find(&id).ok_or("unknown model")?;
    models::download::start(&app, m);
    if m.category == "transcription" {
        models::download_companions(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, id: String) {
    app.state::<models::download::Downloads>().cancel(&id);
}

#[tauri::command]
pub fn delete_model(app: AppHandle, id: String) -> Result<(), String> {
    models::delete(&app, &id)?;
    let _ = app.emit(
        models::EVENT_CHANGED,
        serde_json::json!({ "id": id, "error": null }),
    );
    Ok(())
}

#[tauri::command]
pub fn select_model(app: AppHandle, id: String) -> Result<(), String> {
    models::select(&app, &id)?;
    let _ = app.emit(
        models::EVENT_CHANGED,
        serde_json::json!({ "id": id, "error": null }),
    );
    Ok(())
}

// ---- Processing ----

/// Queues (re)transcription of a meeting. `cpu` forces the CPU backend for this run; `model`
/// transcribes with another downloaded model instead of the selected one.
#[tauri::command]
pub fn transcribe_meeting(
    app: AppHandle,
    id: i64,
    cpu: bool,
    model: Option<String>,
) -> Result<(), String> {
    match &model {
        Some(m) => {
            if !models::find(m)
                .is_some_and(|m| m.category == "transcription" && models::is_ready(&app, m))
            {
                return Err("That model isn't downloaded.".into());
            }
        }
        None if models::selected(&app).is_none() => {
            return Err(
                "Choose and download a transcription model in Settings → Transcription first."
                    .into(),
            );
        }
        None => {}
    }
    let options = crate::processing::JobOptions {
        backend: cpu.then_some(earshot_protocol::Backend::Cpu),
        model,
        ..Default::default()
    };
    crate::processing::enqueue(&app, id, options)
}

/// Separates speakers again from the existing transcript, optionally with a known number of
/// speakers or another sensitivity. Names given to speakers carry over where they clearly match.
#[tauri::command]
pub fn separate_speakers(
    app: AppHandle,
    id: i64,
    speaker_count: Option<u32>,
    sensitivity: Option<f32>,
) -> Result<(), String> {
    if models::diarization_models(&app).is_none() {
        models::download_companions(&app);
        return Err("The speaker models are still downloading. Try again in a minute.".into());
    }
    let options = crate::processing::JobOptions {
        speakers_only: true,
        speaker_count: speaker_count.filter(|&n| (1..=20).contains(&n)),
        sensitivity: sensitivity.map(|s| s.clamp(0.0, 100.0)),
        ..Default::default()
    };
    crate::processing::enqueue(&app, id, options)
}

#[tauri::command]
pub fn cancel_processing(app: AppHandle, id: i64) {
    app.state::<crate::processing::Processor>().cancel(&app, id);
}

#[tauri::command]
pub fn processing_jobs(db: State<'_, Db>) -> Result<Vec<crate::db::JobRow>, String> {
    db.latest_jobs(crate::processing::JOB_KIND).map_err(err)
}

// ---- Summaries ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryState {
    summary: Option<crate::db::SummaryRow>,
    /// The meeting's latest summary job (running, failed, …).
    job: Option<crate::db::JobRow>,
}

#[tauri::command]
pub fn get_summary(db: State<'_, Db>, id: i64) -> Result<SummaryState, String> {
    let job = db
        .latest_jobs(crate::summary::JOB_KIND)
        .map_err(err)?
        .into_iter()
        .find(|j| j.meeting_id == id);
    Ok(SummaryState {
        summary: db.summary(id).map_err(err)?,
        job,
    })
}

/// Latest summary job of each meeting (for "Summarizing…" in the list).
#[tauri::command]
pub fn summary_jobs(db: State<'_, Db>) -> Result<Vec<crate::db::JobRow>, String> {
    db.latest_jobs(crate::summary::JOB_KIND).map_err(err)
}

/// Writes (or rewrites) a meeting's summary, titling the meeting too if it has no title yet.
#[tauri::command]
pub fn summarize_meeting(
    app: AppHandle,
    id: i64,
    template_id: Option<i64>,
    model: Option<String>,
) -> Result<(), String> {
    let db = app.state::<Db>();
    if let Some(m) = &model
        && !models::find(m).is_some_and(|m| m.category == "summary" && models::is_ready(&app, m))
    {
        return Err("That summary model isn't downloaded.".into());
    }
    if model.is_none() && models::summary_model(&app).is_none() {
        return Err("Choose and download a summary model in Settings → Summary.".into());
    }
    let title = db
        .get_setting(crate::summary::AUTO_TITLE_SETTING)
        .map_err(err)?
        .as_deref()
        != Some("false");
    crate::summary::enqueue(
        &app,
        id,
        &crate::summary::SummaryOptions {
            summary: true,
            title,
            template_id,
            model,
            backend: None,
        },
    )
}

#[tauri::command]
pub fn cancel_summary(app: AppHandle, id: i64) {
    app.state::<crate::processing::Processor>()
        .cancel_kind(&app, id, crate::summary::JOB_KIND);
    let _ = app.emit(crate::summary::EVENT_CHANGED, id);
}

#[tauri::command]
pub fn edit_summary(app: AppHandle, id: i64, markdown: String) -> Result<(), String> {
    if markdown.len() > 200_000 {
        return Err("That summary is too long.".into());
    }
    app.state::<Db>().edit_summary(id, &markdown).map_err(err)?;
    let _ = app.emit(crate::summary::EVENT_CHANGED, id);
    Ok(())
}

#[tauri::command]
pub fn list_templates(db: State<'_, Db>) -> Result<Vec<crate::db::TemplateRow>, String> {
    db.templates().map_err(err)
}

/// Creates (no `id`) or updates a template; returns its id.
#[tauri::command]
pub fn save_template(
    db: State<'_, Db>,
    id: Option<i64>,
    name: String,
    instructions: String,
    sections: Vec<String>,
) -> Result<i64, String> {
    db.save_template(id, &name, &instructions, &sections)
        .map_err(err)
}

#[tauri::command]
pub fn delete_template(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db.delete_template(id).map_err(err)
}

#[tauri::command]
pub fn reset_template(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db.reset_template(id).map_err(err)
}

// ---- Speakers and transcript editing ----

#[tauri::command]
pub fn get_speakers(db: State<'_, Db>, id: i64) -> Result<Vec<crate::db::SpeakerRow>, String> {
    db.speakers(id).map_err(err)
}

/// Renames a speaker in one meeting; an empty name goes back to "Speaker N".
#[tauri::command]
pub fn rename_speaker(db: State<'_, Db>, id: i64, name: String) -> Result<(), String> {
    let name: String = name.trim().chars().take(60).collect();
    db.rename_speaker(id, (!name.is_empty()).then_some(name.as_str()))
        .map_err(err)
}

#[tauri::command]
pub fn merge_speakers(db: State<'_, Db>, from: i64, into: i64) -> Result<(), String> {
    db.merge_speakers(from, into).map_err(err)
}

#[tauri::command]
pub fn set_utterance_speaker(db: State<'_, Db>, id: i64, speaker_id: i64) -> Result<(), String> {
    db.set_utterance_speaker(id, speaker_id).map_err(err)
}

#[tauri::command]
pub fn set_utterance_text(db: State<'_, Db>, id: i64, text: String) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("A line can't be empty.".into());
    }
    if text.chars().count() > 10_000 {
        return Err("That line is too long.".into());
    }
    db.set_utterance_text(id, text).map_err(err)
}

// ---- Voice profiles ----

fn profile_name(name: &str) -> Result<String, String> {
    let name: String = name.trim().chars().take(60).collect();
    if name.is_empty() {
        return Err("Give the voice a name.".into());
    }
    Ok(name)
}

#[tauri::command]
pub fn list_profiles(db: State<'_, Db>) -> Result<Vec<crate::db::ProfileRow>, String> {
    db.profiles().map_err(err)
}

#[tauri::command]
pub fn profile_meetings(
    db: State<'_, Db>,
    id: i64,
) -> Result<Vec<crate::db::ProfileMeeting>, String> {
    db.profile_meetings(id).map_err(err)
}

/// Saves a meeting speaker's voice as a new profile named `name`.
#[tauri::command]
pub fn save_voice_profile(db: State<'_, Db>, speaker_id: i64, name: String) -> Result<i64, String> {
    db.create_profile_from_speaker(speaker_id, &profile_name(&name)?)
        .map_err(err)
}

/// Links a meeting speaker to an existing profile (confirming a suggestion, or "this is …"), and
/// adds this meeting's voice to the profile.
#[tauri::command]
pub fn link_voice_profile(
    db: State<'_, Db>,
    speaker_id: i64,
    profile_id: i64,
) -> Result<(), String> {
    db.link_speaker_to_profile(speaker_id, profile_id)
        .map_err(err)
}

/// "Not them": removes a suggested (or confirmed) profile link from a meeting speaker.
#[tauri::command]
pub fn unlink_voice_profile(db: State<'_, Db>, speaker_id: i64) -> Result<(), String> {
    db.unlink_speaker(speaker_id).map_err(err)
}

#[tauri::command]
pub fn rename_profile(db: State<'_, Db>, id: i64, name: String) -> Result<(), String> {
    db.rename_profile(id, &profile_name(&name)?).map_err(err)
}

#[tauri::command]
pub fn delete_profile(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db.delete_profile(id).map_err(err)
}

// ---- Search, export, delete ----

#[tauri::command]
pub fn search_transcripts(
    db: State<'_, Db>,
    query: String,
) -> Result<Vec<crate::db::SearchHit>, String> {
    let query: String = query.chars().take(200).collect();
    db.search(&query, 200).map_err(err)
}

fn me_name(db: &Db) -> String {
    db.get_setting("speakers.meName")
        .ok()
        .flatten()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Me".to_owned())
}

/// Saves the transcript as Markdown, text, SRT, VTT or JSON where the user chooses. `when` (the
/// meeting's local date and time) and `date` (for the file name) come formatted from the page.
/// Returns the saved path, or `None` if the dialog was cancelled.
#[tauri::command]
pub async fn export_meeting(
    app: AppHandle,
    id: i64,
    format: String,
    when: String,
    date: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let format = crate::export::Format::parse(&format).ok_or("unknown export format")?;
    blocking(move || {
        let db = app.state::<Db>();
        let meeting = db.meeting(id).map_err(err)?.ok_or("meeting not found")?;
        let lines = db.transcript(id).map_err(err)?;
        let summary = db.summary(id).map_err(err)?;
        let me = me_name(&db);
        let doc = crate::export::Doc {
            meeting: &meeting,
            lines: &lines,
            me_name: &me,
            when: when.chars().take(100).collect(),
            summary: summary.as_ref().map(|s| s.markdown.as_str()),
        };
        let content = crate::export::render(&doc, format);
        let date: String = date
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .take(10)
            .collect();
        let name = format!(
            "{}.{}",
            crate::export::file_stem(&meeting, &date),
            format.extension()
        );
        let Some(path) = app
            .dialog()
            .file()
            .set_title("Export transcript")
            .set_file_name(&name)
            .add_filter(format.label(), &[format.extension()])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = path.into_path().map_err(err)?;
        std::fs::write(&path, content)
            .map_err(|e| format!("Couldn't save {}: {e}", path.display()))?;
        Ok(Some(path.display().to_string()))
    })
    .await
}

/// Copies the meeting's audio tracks (FLAC) into a folder the user picks.
#[tauri::command]
pub async fn export_audio(app: AppHandle, id: i64, date: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let db = app.state::<Db>();
        let meeting = db.meeting(id).map_err(err)?.ok_or("meeting not found")?;
        let tracks = db.tracks(id).map_err(err)?;
        if tracks.is_empty() {
            return Err("This meeting has no audio.".into());
        }
        let Some(dir) = app
            .dialog()
            .file()
            .set_title("Export audio to folder")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let dir = dir.into_path().map_err(err)?;
        let date: String = date
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .take(10)
            .collect();
        let stem = crate::export::file_stem(&meeting, &date);
        let me = me_name(&db);
        for t in tracks {
            let who = if t.kind == "mic" {
                me.as_str()
            } else {
                "Meeting"
            };
            let who: String = who
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .collect();
            let target = dir.join(format!("{stem} - {who}.flac"));
            std::fs::copy(&t.path, &target)
                .map_err(|e| format!("Couldn't copy to {}: {e}", target.display()))?;
        }
        Ok(Some(dir.display().to_string()))
    })
    .await
}

/// Permanently deletes a meeting: its audio files, transcript and speakers.
#[tauri::command]
pub fn delete_meeting(app: AppHandle, id: i64) -> Result<(), String> {
    if app.state::<Recorder>().state().meeting_id == Some(id) {
        return Err("Stop the recording before deleting this meeting.".into());
    }
    app.state::<crate::processing::Processor>().cancel(&app, id);
    let paths = app.state::<Db>().delete_meeting(id).map_err(err)?;
    for p in &paths {
        let path = std::path::Path::new(p);
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("couldn't delete {}: {e}", path.display());
        }
        // Remove the meeting's own folder once it's empty (never a non-empty or shared one).
        if let Some(dir) = path.parent()
            && dir.file_name().and_then(|n| n.to_str()) == Some(&format!("meeting-{id}"))
        {
            let _ = std::fs::remove_dir(dir);
        }
    }
    log::info!("meeting {id} deleted");
    let _ = app.emit(crate::processing::EVENT_CHANGED, id);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAudio {
    kind: String,
    path: String,
    duration_ms: i64,
    /// Where the recording came from ("Default system audio", a device or an app).
    source: Option<String>,
}

/// The meeting's audio files for the player. Only these files are opened to the webview (the
/// asset protocol scope starts empty), so pages can't read anything else on disk.
#[tauri::command]
pub fn meeting_audio(app: AppHandle, id: i64) -> Result<Vec<TrackAudio>, String> {
    let tracks = app.state::<Db>().tracks(id).map_err(err)?;
    let scope = app.asset_protocol_scope();
    let mut out = Vec::new();
    for t in tracks.into_iter().filter(|t| t.finalized) {
        if !std::path::Path::new(&t.path).is_file() {
            continue;
        }
        scope.allow_file(&t.path).map_err(err)?;
        out.push(TrackAudio {
            kind: t.kind,
            path: t.path,
            duration_ms: t.duration_ms,
            source: t.source,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn get_transcript(db: State<'_, Db>, id: i64) -> Result<Vec<crate::db::UtteranceRow>, String> {
    db.transcript(id).map_err(err)
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

/// Opens a web page (the About screen's credit links) in the default browser.
#[tauri::command]
pub fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !url.starts_with("https://") {
        return Err("Only https links can be opened.".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
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
