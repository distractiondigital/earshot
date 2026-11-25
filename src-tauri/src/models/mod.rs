//! Model manager: the bundled catalog (`models.json`), local model files, selection, and
//! downloads. Models are only downloaded when the user asks; nothing else touches the network.

pub mod download;
pub mod hardware;

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::Db;

pub const EVENT_CHANGED: &str = "models://changed";
pub const EVENT_PROGRESS: &str = "models://progress";
pub const SELECTED_SETTING: &str = "transcription.model";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub category: String,
    pub name: String,
    pub description: String,
    pub file: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// 1–5 (higher is better).
    pub accuracy: u8,
    /// 1–5 (higher is faster).
    pub speed: u8,
    pub dtw_preset: Option<String>,
    pub english_only: bool,
    pub advanced: bool,
    pub badge: Option<String>,
    pub license: String,
}

#[derive(Debug, Deserialize)]
struct Catalog {
    models: Vec<CatalogModel>,
}

pub fn catalog() -> &'static [CatalogModel] {
    static CATALOG: OnceLock<Vec<CatalogModel>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<Catalog>(include_str!("../../models.json"))
            .expect("bundled models.json is valid")
            .models
    })
}

pub fn find(id: &str) -> Option<&'static CatalogModel> {
    catalog().iter().find(|m| m.id == id)
}

/// The VAD model downloaded alongside transcription models.
pub fn vad_model() -> &'static CatalogModel {
    catalog()
        .iter()
        .find(|m| m.category == "vad")
        .expect("catalog has a VAD model")
}

pub fn models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn path_of(app: &AppHandle, m: &CatalogModel) -> Result<PathBuf, String> {
    Ok(models_dir(app)?.join(&m.file))
}

/// A model is ready when its file exists with the catalog size. Files only get their final name
/// after the SHA-256 check passes, so a full-size file is a verified file.
pub fn is_ready(app: &AppHandle, m: &CatalogModel) -> bool {
    path_of(app, m)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|md| md.len() == m.size_bytes)
}

pub fn selected(app: &AppHandle) -> Option<&'static CatalogModel> {
    let id = app
        .state::<Db>()
        .get_setting(SELECTED_SETTING)
        .ok()
        .flatten()?;
    find(&id).filter(|m| m.category == "transcription" && is_ready(app, m))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    #[serde(flatten)]
    pub model: CatalogModel,
    /// "available", "downloading" or "ready".
    pub state: &'static str,
    pub downloaded_bytes: u64,
    pub selected: bool,
    pub recommended: bool,
}

pub fn list(app: &AppHandle, recommended: &str) -> Vec<ModelView> {
    let downloads = app.state::<download::Downloads>();
    let selected_id = selected(app).map(|m| m.id.clone());
    catalog()
        .iter()
        .map(|m| {
            let progress = downloads.progress(&m.id);
            let state = if progress.is_some() {
                "downloading"
            } else if is_ready(app, m) {
                "ready"
            } else {
                "available"
            };
            ModelView {
                model: m.clone(),
                state,
                downloaded_bytes: progress.unwrap_or(0),
                selected: selected_id.as_deref() == Some(m.id.as_str()),
                recommended: m.id == recommended,
            }
        })
        .collect()
}

pub fn select(app: &AppHandle, id: &str) -> Result<(), String> {
    let m = find(id).ok_or("unknown model")?;
    if m.category != "transcription" || !is_ready(app, m) {
        return Err(format!("{} is not downloaded", m.name));
    }
    app.state::<Db>()
        .set_setting(SELECTED_SETTING, id)
        .map_err(|e| e.to_string())
}

pub fn delete(app: &AppHandle, id: &str) -> Result<(), String> {
    let m = find(id).ok_or("unknown model")?;
    app.state::<download::Downloads>().cancel(id);
    let path = path_of(app, m)?;
    for p in [path.clone(), download::part_path(&path)] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot delete {}: {e}", p.display())),
        }
    }
    let db = app.state::<Db>();
    if db.get_setting(SELECTED_SETTING).ok().flatten().as_deref() == Some(id) {
        db.set_setting(SELECTED_SETTING, "")
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        let c = catalog();
        assert!(c.iter().filter(|m| m.category == "transcription").count() >= 6);
        assert_eq!(c.iter().filter(|m| m.category == "vad").count(), 1);
        let mut ids: Vec<_> = c.iter().map(|m| &m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.len(), "duplicate ids");
        for m in c {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(
                m.sha256.chars().all(|ch| ch.is_ascii_hexdigit()),
                "{}",
                m.id
            );
            assert!(
                m.url.starts_with("https://huggingface.co/") && m.url.ends_with(&m.file),
                "{}",
                m.id
            );
            // Pinned to a commit, not a moving branch.
            assert!(!m.url.contains("/resolve/main/"), "{}", m.id);
            assert!(m.size_bytes > 0);
        }
        // Every recommendation target exists.
        for id in [
            "large-v3-turbo",
            "large-v3-turbo-q5_0",
            "small-q5_1",
            "base-q5_1",
        ] {
            assert!(find(id).is_some(), "{id}");
        }
    }
}
