mod commands;
mod db;
mod hotkey;
mod pill;
mod recorder;
mod tray;
mod worker;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::db::Db;
use crate::recorder::Recorder;

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
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            tray::build(app.handle())?;
            hotkey::register_saved(app.handle());
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
            commands::list_sources,
            commands::list_meetings,
            commands::update_meeting,
            commands::recover_meeting,
            commands::recordings_dir,
            commands::choose_recordings_dir,
            commands::open_recordings_dir,
            commands::get_hotkey,
            commands::set_hotkey,
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
