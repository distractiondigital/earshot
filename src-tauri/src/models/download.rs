//! Resumable, SHA-256-verified model downloads over HTTPS.
//!
//! Data goes to `<file>.part`; an interrupted download resumes with an HTTP Range request. The
//! file only gets its final name after the hash matches the catalog, so a partial or corrupt
//! download is never mistaken for a ready model.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{CatalogModel, EVENT_CHANGED, EVENT_PROGRESS, SELECTED_SETTING, SUMMARY_SETTING};
use crate::db::Db;

struct Handle {
    cancel: AtomicBool,
    downloaded: AtomicU64,
}

/// Downloads in progress (managed state).
#[derive(Default)]
pub struct Downloads {
    active: Mutex<HashMap<String, Arc<Handle>>>,
}

impl Downloads {
    pub fn progress(&self, id: &str) -> Option<u64> {
        self.active
            .lock()
            .unwrap()
            .get(id)
            .map(|h| h.downloaded.load(Ordering::Relaxed))
    }

    pub fn cancel(&self, id: &str) {
        if let Some(h) = self.active.lock().unwrap().get(id) {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    id: &'a str,
    downloaded: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Changed<'a> {
    id: &'a str,
    /// Set when a download failed.
    error: Option<String>,
}

pub fn part_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Starts downloading `m` in the background (no-op if already downloading or ready).
pub fn start(app: &AppHandle, m: &'static CatalogModel) {
    if super::is_ready(app, m) {
        return;
    }
    let handle = {
        let downloads = app.state::<Downloads>();
        let mut active = downloads.active.lock().unwrap();
        if active.contains_key(&m.id) {
            return;
        }
        let h = Arc::new(Handle {
            cancel: AtomicBool::new(false),
            downloaded: AtomicU64::new(0),
        });
        active.insert(m.id.clone(), h.clone());
        h
    };
    let _ = app.emit(
        EVENT_CHANGED,
        Changed {
            id: &m.id,
            error: None,
        },
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = run(&app, m, &handle).await;
        app.state::<Downloads>()
            .active
            .lock()
            .unwrap()
            .remove(&m.id);
        let error = match result {
            Ok(()) => {
                log::info!("model {} downloaded and verified", m.id);
                // First transcription or summary model: select it automatically.
                let db = app.state::<Db>();
                let key = match m.category.as_str() {
                    "transcription" => Some(SELECTED_SETTING),
                    "summary" => Some(SUMMARY_SETTING),
                    _ => None,
                };
                if let Some(key) = key
                    && db
                        .get_setting(key)
                        .ok()
                        .flatten()
                        .is_none_or(|s| s.is_empty())
                {
                    let _ = db.set_setting(key, &m.id);
                }
                None
            }
            Err(e) if handle.cancel.load(Ordering::Relaxed) => {
                log::info!("model {} download cancelled: {e}", m.id);
                None
            }
            Err(e) => {
                log::error!("model {} download failed: {e}", m.id);
                Some(e)
            }
        };
        let _ = app.emit(EVENT_CHANGED, Changed { id: &m.id, error });
    });
}

async fn run(app: &AppHandle, m: &CatalogModel, h: &Handle) -> Result<(), String> {
    let path = super::path_of(app, m)?;
    let part = part_path(&path);

    // Resume: hash what is already on disk.
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if let Ok(mut f) = tokio::fs::File::open(&part).await {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }
    if have > m.size_bytes {
        // Stale or foreign file: start over.
        have = 0;
        hasher = Sha256::new();
    }
    h.downloaded.store(have, Ordering::Relaxed);

    let client = reqwest::Client::builder()
        .user_agent(concat!("Earshot/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&m.url);
    if have > 0 && have < m.size_bytes {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = if have == m.size_bytes {
        None // already complete; just verify
    } else {
        let r = req
            .send()
            .await
            .map_err(|e| format!("download failed: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("download failed: HTTP {}", r.status()));
        }
        Some(r)
    };

    if let Some(resp) = resp {
        let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !resumed && have > 0 {
            // Server ignored the Range request: restart from zero.
            have = 0;
            hasher = Sha256::new();
            h.downloaded.store(0, Ordering::Relaxed);
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .await
            .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut stream = resp.bytes_stream();
        let mut last_emit = Instant::now();
        while let Some(chunk) = stream.next().await {
            if h.cancel.load(Ordering::Relaxed) {
                file.flush().await.ok();
                return Err("cancelled".into());
            }
            let chunk = chunk.map_err(|e| format!("download interrupted: {e}"))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("cannot write model file: {e}"))?;
            hasher.update(&chunk);
            have += chunk.len() as u64;
            h.downloaded.store(have, Ordering::Relaxed);
            if have > m.size_bytes {
                return Err("the download is larger than expected".into());
            }
            if last_emit.elapsed() >= Duration::from_millis(250) {
                last_emit = Instant::now();
                let _ = app.emit(
                    EVENT_PROGRESS,
                    Progress {
                        id: &m.id,
                        downloaded: have,
                        total: m.size_bytes,
                    },
                );
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(|e| e.to_string())?;
    }

    if have != m.size_bytes {
        return Err(format!(
            "download incomplete ({have} of {} bytes)",
            m.size_bytes
        ));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != m.sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(
            "the downloaded file failed its integrity check and was removed; try again".into(),
        );
    }
    tokio::fs::rename(&part, &path)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        EVENT_PROGRESS,
        Progress {
            id: &m.id,
            downloaded: have,
            total: m.size_bytes,
        },
    );
    Ok(())
}
