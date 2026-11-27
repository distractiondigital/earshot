mod commands;
mod db;
mod export;
mod hotkey;
mod mixdown;
mod models;
mod pill;
mod processing;
mod profiles;
mod recorder;
mod speakers;
mod summary;
mod tray;
mod worker;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::db::Db;
use crate::recorder::Recorder;

/// The updater. Builds of another flavor than the default for their platform (the Windows CUDA
/// build) are compiled with `EARSHOT_UPDATER_TARGET` set to their own key in latest.json
/// (e.g. "windows-x86_64-cuda"), so each installer only ever updates to the same flavor.
fn updater<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R, tauri_plugin_updater::Config> {
    let builder = tauri_plugin_updater::Builder::new();
    match option_env!("EARSHOT_UPDATER_TARGET") {
        Some(target) if !target.is_empty() => builder.target(target),
        _ => builder,
    }
    .build()
}

/// The build flavor shown in About ("CUDA" for the NVIDIA build), if not the standard one.
pub fn build_flavor() -> Option<&'static str> {
    match option_env!("EARSHOT_UPDATER_TARGET") {
        Some(t) if t.ends_with("-cuda") => Some("CUDA"),
        _ => None,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // Must be first: a second launch focuses the running instance instead of starting a
        // second recorder.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        // Local log files only (app log dir), plus stdout in dev. Never sent anywhere.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(2_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(5))
                .build(),
        )
        .plugin(updater())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(hotkey::plugin())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = Db::open(&data_dir.join("earshot.db"))?;
            let interrupted = db.mark_interrupted()?;
            if interrupted > 0 {
                log::warn!("{interrupted} recording(s) were interrupted and can be recovered");
            }
            app.manage(db);
            app.manage(Recorder::default());
            app.manage(models::download::Downloads::default());
            let processor = processing::Processor::start(app.handle());
            app.manage(processor);
            tray::build(app.handle())?;
            hotkey::register_saved(app.handle());
            // Speaker models arrived after some transcription models were downloaded (M3):
            // fetch the small helper models for anyone who already set up transcription.
            if models::selected(app.handle()).is_some() {
                models::download_companions(app.handle());
            }
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
        .invoke_handler(tauri::generate_handler![
            commands::get_setting,
            commands::set_setting,
            commands::app_info,
            commands::recorder_state,
            commands::start_recording,
            commands::stop_recording,
            commands::resume_recording,
            commands::interrupted_meeting,
            commands::pause_recording,
            commands::switch_recording_source,
            commands::show_recorder,
            commands::list_sources,
            commands::list_meetings,
            commands::update_meeting,
            commands::recover_meeting,
            commands::recordings_dir,
            commands::choose_recordings_dir,
            commands::open_recordings_dir,
            commands::open_link,
            commands::reveal_licenses,
            commands::get_hotkey,
            commands::set_hotkey,
            commands::list_models,
            commands::download_model,
            commands::cancel_download,
            commands::delete_model,
            commands::select_model,
            commands::transcribe_meeting,
            commands::separate_speakers,
            commands::cancel_processing,
            commands::processing_jobs,
            commands::get_transcript,
            commands::meeting_audio,
            commands::list_profiles,
            commands::profile_meetings,
            commands::save_voice_profile,
            commands::link_voice_profile,
            commands::unlink_voice_profile,
            commands::rename_profile,
            commands::delete_profile,
            commands::search_transcripts,
            commands::export_meeting,
            commands::export_audio,
            commands::delete_meeting,
            commands::get_speakers,
            commands::rename_speaker,
            commands::merge_speakers,
            commands::set_utterance_speaker,
            commands::set_utterance_text,
            commands::get_summary,
            commands::summary_jobs,
            commands::summarize_meeting,
            commands::cancel_summary,
            commands::edit_summary,
            commands::list_templates,
            commands::save_template,
            commands::delete_template,
            commands::reset_template,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Earshot");

    app.run(|app, event| {
        // Quitting (tray, OS shutdown, updater) finalizes an active recording first.
        if let RunEvent::ExitRequested { .. } = event {
            let recorder = app.state::<Recorder>();
            if recorder.is_active()
                && let Err(e) = recorder.stop(app)
            {
                log::error!("failed to finalize recording on exit: {e}");
            }
        }
    });
}
