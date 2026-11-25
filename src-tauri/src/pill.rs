//! Floating recorder pill: a tiny always-on-top window shown while recording.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::db::Db;

const LABEL: &str = "pill";
const WIDTH: f64 = 248.0;
const HEIGHT: f64 = 44.0;

fn enabled(app: &AppHandle) -> bool {
    app.state::<Db>()
        .get_setting("recording.showPill")
        .ok()
        .flatten()
        .as_deref()
        != Some("false")
}

pub fn show(app: &AppHandle) {
    if !enabled(app) {
        return;
    }
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.show();
        return;
    }
    let mut builder =
        WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?view=pill".into()))
            .title("Earshot recorder")
            .inner_size(WIDTH, HEIGHT)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible_on_all_workspaces(true);

    // Top centre of the primary monitor, just below the menu bar / screen edge.
    if let Ok(Some(m)) = app.primary_monitor() {
        let scale = m.scale_factor();
        let size = m.size().to_logical::<f64>(scale);
        let pos = m.position().to_logical::<f64>(scale);
        builder = builder.position(pos.x + (size.width - WIDTH) / 2.0, pos.y + 40.0);
    }
    if let Err(e) = builder.build() {
        log::error!("failed to open recorder pill: {e}");
    }
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    }
}
