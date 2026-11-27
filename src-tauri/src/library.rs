//! Meeting folders. Each meeting is saved as `meeting.json` in its own folder next to its audio,
//! with a readable `Transcript.md`, so a recordings folder kept in cloud storage (Nextcloud,
//! Google Drive, Dropbox, OneDrive, iCloud Drive) carries whole meetings between computers.
//!
//! The database stays the local index (search, the meeting list, processing jobs). Changes are
//! written out a moment after they happen ([`flush`]); folders are read in when another computer
//! added or changed them, and dropped when another computer deleted them ([`scan`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::Db;

/// The meeting's data, next to its audio.
pub const FILE: &str = "meeting.json";
/// The transcript and summary for reading in any app (written, never read).
pub const READABLE: &str = "Transcript.md";
/// Written first, then renamed over [`FILE`]; sync clients skip `*.part` files.
const PARTIAL: &str = "meeting.json.part";
/// Format of [`MeetingFile`]. A file from a newer version is left alone.
pub const FORMAT: u32 = 1;

const FLUSH_EVERY: Duration = Duration::from_secs(2);
const SCAN_EVERY: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingFile {
    pub format: u32,
    /// Identifies the meeting on every computer.
    pub uid: String,
    /// The computer that recorded it.
    #[serde(default)]
    pub device: Option<String>,
    /// When this file was written (ms since the epoch); the newer side wins a conflict.
    #[serde(default)]
    pub saved_at: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub title_generated: bool,
    pub started_at: i64,
    #[serde(default)]
    pub duration_ms: i64,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub speaker_count_hint: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tracks: Vec<FileTrack>,
    #[serde(default)]
    pub speakers: Vec<FileSpeaker>,
    #[serde(default)]
    pub utterances: Vec<FileUtterance>,
    #[serde(default)]
    pub summary: Option<FileSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTrack {
    /// "mic" or "system".
    pub kind: String,
    /// File name in the meeting's folder.
    pub file: String,
    pub sample_rate: i64,
    pub channels: i64,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub finalized: bool,
    #[serde(default)]
    pub start_offset_ms: i64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_spec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSpeaker {
    /// "Me" or "Speaker N".
    pub label: String,
    /// Name given in this meeting.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_me: bool,
    #[serde(default)]
    pub color: Option<String>,
    /// Name of the voice profile it's linked to; profiles themselves stay on each computer and
    /// are matched by name.
    #[serde(default)]
    pub profile: Option<String>,
    /// The profile link is a suggestion that hasn't been confirmed.
    #[serde(default)]
    pub auto_matched: bool,
    #[serde(default)]
    pub voice: Option<FileVoice>,
}

/// A speaker's voice embedding, so it can be saved as a profile on any computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVoice {
    pub model: String,
    pub vector: Vec<f32>,
    #[serde(default)]
    pub speech_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUtterance {
    /// Track kind.
    pub track: String,
    /// Index into `speakers`.
    #[serde(default)]
    pub speaker: Option<usize>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    #[serde(default)]
    pub edited: bool,
    /// `[start_ms, end_ms, text, probability]`.
    #[serde(default)]
    pub words: Vec<(i64, i64, String, Option<f64>)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    pub markdown: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub edited: bool,
}

pub fn new_uid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Folder for a new meeting: local start time plus part of its id, e.g. `2026-11-27 14.05 3f9a2c1d`
/// (never the same on two computers, so meetings in a shared folder can't collide).
pub fn folder_name(started_at: i64, uid: &str) -> String {
    let when = chrono::DateTime::from_timestamp_millis(started_at)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H.%M")
                .to_string()
        })
        .unwrap_or_default();
    let short: String = uid.chars().take(8).collect();
    format!("{when} {short}").trim().to_owned()
}

/// Hash of what a file says, ignoring when it was written.
pub fn content_hash(f: &MeetingFile) -> String {
    let mut f = f.clone();
    f.saved_at = 0;
    let bytes = serde_json::to_vec(&f).expect("serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Size and modified time of a file, to tell cheaply whether it changed.
fn stamp(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let modified = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}:{modified}", m.len()))
}

/// Reads a meeting folder's file. `Ok(None)` when there is none.
pub fn read(dir: &Path) -> Result<Option<MeetingFile>, String> {
    let path = dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let f: MeetingFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if f.format > FORMAT {
        return Err(format!(
            "{} was saved by a newer version of Earshot",
            path.display()
        ));
    }
    if f.uid.trim().is_empty() {
        return Err(format!("{} has no meeting id", path.display()));
    }
    Ok(Some(f))
}

/// Writes the file (and the readable copy) into `dir`, replacing what was there in one step.
/// Returns the new file's stamp.
pub fn write(dir: &Path, f: &MeetingFile, readable: Option<&str>) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(f).expect("serializes");
    let part = dir.join(PARTIAL);
    let path = dir.join(FILE);
    std::fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    std::fs::rename(&part, &path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("{}: {e}", path.display())
    })?;
    let md = dir.join(READABLE);
    match readable {
        Some(text) => {
            if std::fs::read_to_string(&md).ok().as_deref() != Some(text)
                && let Err(e) = std::fs::write(&md, text)
            {
                log::warn!("couldn't write {}: {e}", md.display());
            }
        }
        None => {
            let _ = std::fs::remove_file(&md);
        }
    }
    stamp(&path).ok_or_else(|| format!("{} vanished after writing", path.display()))
}

/// Removes what Earshot keeps in a meeting's folder (audio included), then the folder if that
/// leaves it empty; anything else the user put there stays.
pub fn remove_folder(dir: &Path, track_paths: &[String]) {
    let mut files: Vec<PathBuf> = track_paths.iter().map(PathBuf::from).collect();
    files.extend([FILE, READABLE, PARTIAL].map(|f| dir.join(f)));
    for path in files {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("couldn't delete {}: {e}", path.display());
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Meetings read in from their folders, and ones removed because their folder is gone.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub changed: Vec<i64>,
    pub removed: Vec<i64>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Writes `meeting.json` for each meeting changed since it was last saved. When another
/// computer changed the same file meanwhile, the newer change wins: a newer file is read in
/// (reported in `changed`) instead of being overwritten.
pub fn flush(db: &Db, readable: &dyn Fn(i64) -> Option<String>) -> Changes {
    let mut out = Changes::default();
    let dirty = match db.library_dirty() {
        Ok(d) => d,
        Err(e) => {
            log::error!("library: {e}");
            return out;
        }
    };
    for (id, changed_at) in dirty {
        if let Err(e) = flush_one(db, id, changed_at, readable, &mut out) {
            log::warn!("couldn't save meeting {id} to its folder: {e}");
        }
    }
    out
}

fn flush_one(
    db: &Db,
    id: i64,
    changed_at: i64,
    readable: &dyn Fn(i64) -> Option<String>,
    out: &mut Changes,
) -> Result<(), String> {
    let done = || {
        db.clear_library_dirty(id, changed_at)
            .map_err(|e| e.to_string())
    };
    let Some(entry) = db.library_entry(id).map_err(|e| e.to_string())? else {
        return done();
    };
    // A meeting being recorded is saved once it stops (which marks it changed again); one
    // without a folder never had audio.
    let (Some(folder), false) = (entry.folder.as_deref(), entry.status == "recording") else {
        return done();
    };
    let dir = Path::new(folder);
    let Some(mut file) = db.meeting_file(id).map_err(|e| e.to_string())? else {
        return done();
    };
    let hash = content_hash(&file);
    let on_disk = dir.join(FILE);
    let disk_stamp = stamp(&on_disk);
    if entry.file_hash.as_deref() == Some(hash.as_str()) && disk_stamp.is_some() {
        return done();
    }
    // Changed on disk since this computer last wrote or read it?
    if disk_stamp.is_some() && disk_stamp != entry.file_stamp {
        match read(dir) {
            Ok(Some(theirs)) if theirs.uid != entry.uid => {
                log::warn!(
                    "{} belongs to another meeting; meeting {id} isn't saved there",
                    on_disk.display()
                );
                return done();
            }
            Ok(Some(theirs))
                if entry.file_hash.as_deref() != Some(content_hash(&theirs).as_str())
                    && theirs.saved_at > changed_at =>
            {
                let new_hash = content_hash(&theirs);
                db.import_meeting_file(&theirs, dir, &new_hash, disk_stamp.as_deref())
                    .map_err(|e| e.to_string())?;
                log::info!("meeting {id} was changed on another computer; kept that version");
                out.changed.push(id);
                return Ok(());
            }
            Ok(_) => {}
            // Unreadable (perhaps half-synced): ours is newer anyway, so replace it.
            Err(e) => log::warn!("{e}"),
        }
    }
    file.saved_at = crate::db::now_ms();
    let stamp = write(dir, &file, readable(id).as_deref())?;
    db.set_library_file(id, &hash, &stamp)
        .map_err(|e| e.to_string())?;
    done()
}

/// Reads meeting folders under `root` that are new here or were changed on another computer,
/// and drops meetings whose folder under `root` was deleted.
pub fn scan(db: &Db, root: &Path) -> Changes {
    let mut out = Changes::default();
    let Ok(dirs) = std::fs::read_dir(root) else {
        return out;
    };
    let entries = match db.library_entries() {
        Ok(e) => e,
        Err(e) => {
            log::error!("library: {e}");
            return out;
        }
    };
    let by_uid: HashMap<&str, &crate::db::LibraryEntry> =
        entries.iter().map(|e| (e.uid.as_str(), e)).collect();
    let by_folder: HashMap<PathBuf, &crate::db::LibraryEntry> = entries
        .iter()
        .filter_map(|e| Some((PathBuf::from(e.folder.as_deref()?), e)))
        .collect();
    let dirty: HashMap<i64, i64> = db.library_dirty().unwrap_or_default().into_iter().collect();

    let mut seen_dirs = 0usize;
    for d in dirs.flatten() {
        let dir = d.path();
        if !dir.is_dir() {
            continue;
        }
        seen_dirs += 1;
        let Some(disk_stamp) = stamp(&dir.join(FILE)) else {
            continue;
        };
        if by_folder
            .get(&dir)
            .is_some_and(|e| e.file_stamp.as_deref() == Some(disk_stamp.as_str()))
        {
            continue;
        }
        let file = match read(&dir) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("{e}");
                continue;
            }
        };
        let hash = content_hash(&file);
        let known = by_uid.get(file.uid.as_str()).copied();
        if let Some(e) = known {
            let same_folder = e.folder.as_deref().map(Path::new) == Some(dir.as_path());
            if !same_folder
                && e.folder
                    .as_deref()
                    .is_some_and(|f| Path::new(f).join(FILE).exists())
            {
                // A copy of a meeting that is still in its own folder.
                continue;
            }
            if e.file_hash.as_deref() == Some(hash.as_str()) && same_folder {
                let _ = db.set_library_file(e.id, &hash, &disk_stamp);
                continue;
            }
            // Leave it while it's busy here, or when this computer's unsaved change is newer
            // (the next flush writes it out).
            let busy = e.status == "recording" || db.has_active_job(e.id).unwrap_or(true);
            if busy || dirty.get(&e.id).is_some_and(|&at| at >= file.saved_at) {
                continue;
            }
        } else if let Some(other) = by_folder.get(&dir) {
            log::warn!(
                "{} holds a different meeting than meeting {}; skipped",
                dir.join(FILE).display(),
                other.id
            );
            continue;
        }
        match db.import_meeting_file(&file, &dir, &hash, Some(&disk_stamp)) {
            Ok(id) => {
                log::info!(
                    "meeting {id} {} from {}",
                    if known.is_some() { "updated" } else { "added" },
                    dir.display()
                );
                out.changed.push(id);
            }
            Err(e) => log::warn!("couldn't read in {}: {e}", dir.display()),
        }
    }

    // Folders deleted on another computer. An empty listing more likely means the cloud folder
    // isn't available right now, so nothing is removed then.
    if seen_dirs > 0 {
        for e in &entries {
            let Some(folder) = e.folder.as_deref().map(Path::new) else {
                continue;
            };
            if e.file_hash.is_none() || !folder.starts_with(root) || folder == root {
                continue;
            }
            if folder.exists() {
                if !folder.join(FILE).exists() {
                    // Its file was removed by hand: write it again.
                    let _ = db.forget_library_file(e.id);
                }
                continue;
            }
            if e.status == "recording" || db.has_active_job(e.id).unwrap_or(true) {
                continue;
            }
            match db.delete_meeting(e.id) {
                Ok(_) => {
                    log::info!("meeting {} removed: its folder was deleted", e.id);
                    out.removed.push(e.id);
                }
                Err(err) => log::warn!("couldn't remove meeting {}: {err}", e.id),
            }
        }
    }
    out
}

/// Runs saving and reading in the background; [`Library::sync`] asks for a scan now.
pub struct Library {
    wake: Mutex<mpsc::Sender<()>>,
}

impl Library {
    pub fn start(app: &AppHandle) -> Self {
        let (wake, rx) = mpsc::channel();
        let app = app.clone();
        std::thread::Builder::new()
            .name("earshot-library".into())
            .spawn(move || run(app, rx))
            .expect("spawn library thread");
        Self {
            wake: Mutex::new(wake),
        }
    }

    pub fn sync(&self) {
        let _ = self.wake.lock().unwrap_or_else(|e| e.into_inner()).send(());
    }
}

fn run(app: AppHandle, rx: mpsc::Receiver<()>) {
    let db = app.state::<Db>();
    if let Err(e) = db.prepare_library() {
        log::error!("library: {e}");
    }
    let readable = |id: i64| crate::commands::readable_transcript(&db, id);
    let mut last_scan: Option<Instant> = None;
    loop {
        let poked = match rx.recv_timeout(FLUSH_EVERY) {
            Ok(()) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut changes = flush(&db, &readable);
        if poked || last_scan.is_none_or(|t| t.elapsed() >= SCAN_EVERY) {
            last_scan = Some(Instant::now());
            match crate::recorder::recordings_dir(&app) {
                Ok(root) => {
                    let found = scan(&db, &root);
                    changes.changed.extend(found.changed);
                    changes.removed.extend(found.removed);
                }
                Err(e) => log::warn!("library: {e}"),
            }
        }
        if !changes.is_empty() {
            for id in changes.changed.iter().chain(&changes.removed) {
                let _ = app.emit(crate::processing::EVENT_CHANGED, *id);
                let _ = app.emit(crate::summary::EVENT_CHANGED, *id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewSpeaker, NewUtterance, NewWord};

    /// A folder shared by two "computers" (two databases).
    struct Shared(PathBuf);

    impl Shared {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("earshot-library-{}", new_uid()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Shared {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn md(_: i64) -> Option<String> {
        Some("# Notes\n".into())
    }

    fn speaker(label: &str, is_me: bool, name: Option<&str>) -> NewSpeaker {
        NewSpeaker {
            label: label.into(),
            is_me,
            name: name.map(Into::into),
            embedding: (!is_me).then(|| ("voice-model".to_owned(), vec![0.25, -0.5, 1.0 / 3.0])),
            speech_ms: 4_000,
            profile_id: None,
            auto_matched: false,
        }
    }

    fn utterance(speaker: usize, start_ms: i64, text: &str) -> NewUtterance {
        NewUtterance {
            speaker: Some(speaker),
            start_ms,
            end_ms: start_ms + 900,
            text: text.into(),
            words: vec![NewWord {
                start_ms,
                end_ms: start_ms + 900,
                text: format!(" {text}"),
                prob: 0.87,
            }],
        }
    }

    /// Records a meeting on `db` the way the recorder does, with a transcript and a summary.
    fn record(db: &Db, root: &Path) -> i64 {
        let started = 1_790_000_000_000;
        let id = db.create_meeting(started, "").unwrap();
        let dir = root.join(folder_name(started, &db.meeting_uid(id).unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        db.set_meeting_folder(id, &dir).unwrap();
        for kind in ["mic", "system"] {
            let path = dir.join(format!("{kind}.flac"));
            std::fs::write(&path, b"fLaC").unwrap();
            let t = db
                .add_track(
                    id,
                    kind,
                    &path.display().to_string(),
                    48_000,
                    1,
                    "Device",
                    None,
                )
                .unwrap();
            db.finalize_track(t, 61_000).unwrap();
        }
        db.set_meeting_status(id, "ready", Some(61_000), None)
            .unwrap();
        db.update_meeting_details(id, "Weekly sync", None, &["team".into()])
            .unwrap();
        let speakers = [
            speaker("Me", true, None),
            speaker("Speaker 1", false, Some("Elena")),
        ];
        let mic = [utterance(0, 1_000, "Morning all.")];
        let system = [utterance(1, 2_500, "Hi, shall we start?")];
        db.save_transcript(id, &speakers, &[("mic", &mic), ("system", &system)])
            .unwrap();
        let template = db.templates().unwrap().remove(0);
        db.save_summary(id, &template, "summary-model", "## Summary\nAll good.")
            .unwrap();
        id
    }

    fn only(db: &Db) -> crate::db::MeetingRow {
        let mut all = db.list_meetings().unwrap();
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    #[test]
    fn meeting_travels_between_computers() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        let id = record(&pc, &shared.0);
        let folder = PathBuf::from(pc.meeting_folder(id).unwrap().unwrap());

        // The PC writes everything to the meeting's folder.
        assert!(flush(&pc, &md).is_empty());
        assert!(folder.join(FILE).exists());
        assert_eq!(
            std::fs::read_to_string(folder.join(READABLE)).unwrap(),
            "# Notes\n"
        );
        assert!(pc.library_dirty().unwrap().is_empty());

        // The Mac finds it in the shared folder, with everything that was in the PC's database.
        let found = scan(&mac, &shared.0);
        assert_eq!(found.changed.len(), 1);
        assert!(found.removed.is_empty());
        let m = only(&mac);
        assert!(m.elsewhere && !only(&pc).elsewhere);
        assert_eq!(
            (m.title.as_str(), m.status.as_str(), m.duration_ms),
            ("Weekly sync", "ready", 61_000)
        );
        assert_eq!(m.tags, ["team"]);
        assert_eq!(m.speakers, ["Elena"]);
        let lines = mac.transcript(m.id).unwrap();
        let text: Vec<_> = lines.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(text, ["Morning all.", "Hi, shall we start?"]);
        assert_eq!(lines[1].speaker_name.as_deref(), Some("Elena"));
        assert_eq!(lines[1].words[0].text, " Hi, shall we start?");
        assert_eq!(
            mac.summary(m.id).unwrap().unwrap().markdown,
            "## Summary\nAll good."
        );
        let tracks = mac.tracks(m.id).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(PathBuf::from(&tracks[0].path), folder.join("mic.flac"));
        // Nothing is lost on the way: the Mac would save exactly what the PC saved.
        assert_eq!(
            content_hash(&mac.meeting_file(m.id).unwrap().unwrap()),
            content_hash(&pc.meeting_file(id).unwrap().unwrap())
        );
        // Reading it in doesn't count as a change to save, and an unchanged folder isn't read
        // again.
        assert!(mac.library_dirty().unwrap().is_empty());
        assert!(scan(&mac, &shared.0).is_empty());

        // A name given on the Mac reaches the PC.
        let elena = mac.speakers(m.id).unwrap()[1].id;
        mac.rename_speaker(elena, Some("Elena Ruiz")).unwrap();
        assert!(flush(&mac, &md).is_empty());
        let back = scan(&pc, &shared.0);
        assert_eq!(back.changed, [id]);
        assert_eq!(only(&pc).speakers, ["Elena Ruiz"]);
        assert!(!only(&pc).elsewhere);
        assert!(flush(&pc, &md).is_empty());
        assert!(scan(&pc, &shared.0).is_empty());
    }

    #[test]
    fn deleting_a_meeting_removes_it_everywhere() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        let id = record(&pc, &shared.0);
        flush(&pc, &md);
        scan(&mac, &shared.0);
        let folder = PathBuf::from(pc.meeting_folder(id).unwrap().unwrap());
        // Something else of the user's in the shared folder keeps it from looking unavailable.
        std::fs::create_dir_all(shared.0.join("Other")).unwrap();

        let paths = pc.delete_meeting(id).unwrap();
        remove_folder(&folder, &paths);
        assert!(!folder.exists());
        let gone = scan(&mac, &shared.0);
        assert_eq!(gone.removed.len(), 1);
        assert!(mac.list_meetings().unwrap().is_empty());
    }

    #[test]
    fn an_unavailable_folder_removes_nothing() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        record(&pc, &shared.0);
        flush(&pc, &md);
        scan(&mac, &shared.0);
        // The cloud folder shows up empty (not synced, drive not mounted): keep the meetings.
        std::fs::remove_dir_all(&shared.0).unwrap();
        std::fs::create_dir_all(&shared.0).unwrap();
        assert!(scan(&mac, &shared.0).is_empty());
        assert_eq!(mac.list_meetings().unwrap().len(), 1);
    }

    #[test]
    fn a_hand_deleted_file_is_written_again() {
        let shared = Shared::new();
        let pc = Db::open_in_memory().unwrap();
        let id = record(&pc, &shared.0);
        flush(&pc, &md);
        let file = PathBuf::from(pc.meeting_folder(id).unwrap().unwrap()).join(FILE);
        std::fs::remove_file(&file).unwrap();
        assert!(scan(&pc, &shared.0).is_empty());
        flush(&pc, &md);
        assert!(file.exists());
    }

    #[test]
    fn the_newer_change_wins() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        let id = record(&pc, &shared.0);
        flush(&pc, &md);
        scan(&mac, &shared.0);
        let mac_id = only(&mac).id;
        let wait = || std::thread::sleep(Duration::from_millis(5));

        // The PC renames first, but the Mac saves a later rename before the PC saves.
        pc.update_meeting_details(id, "PC title", None, &[])
            .unwrap();
        wait();
        mac.update_meeting_details(mac_id, "Mac title", None, &[])
            .unwrap();
        flush(&mac, &md);
        // The PC's scan leaves the file for its flush, which keeps the Mac's newer version.
        let kept = flush(&pc, &md);
        assert_eq!(kept.changed, [id]);
        assert_eq!(only(&pc).title, "Mac title");
        assert!(pc.library_dirty().unwrap().is_empty());

        // Now the Mac saves first and the PC renames afterwards: the PC's newer change is kept
        // and overwrites the file.
        mac.update_meeting_details(mac_id, "Mac again", None, &[])
            .unwrap();
        flush(&mac, &md);
        wait();
        pc.update_meeting_details(id, "PC later", None, &[])
            .unwrap();
        assert!(scan(&pc, &shared.0).is_empty());
        assert!(flush(&pc, &md).is_empty());
        assert_eq!(only(&pc).title, "PC later");
        scan(&mac, &shared.0);
        assert_eq!(only(&mac).title, "PC later");
    }

    #[test]
    fn only_the_recording_computer_can_continue_an_interrupted_meeting() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        let id = record(&pc, &shared.0);
        pc.set_meeting_status(id, "incomplete", None, None).unwrap();
        flush(&pc, &md);
        scan(&mac, &shared.0);
        let m = only(&mac);
        assert_eq!((m.status.as_str(), m.elsewhere), ("incomplete", true));
        assert!(crate::recorder::interrupted(&mac).is_none());
        assert!(crate::recorder::recover(&mac, m.id).is_err());
        assert_eq!(crate::recorder::interrupted(&pc).unwrap().meeting_id, id);
    }

    #[test]
    fn profiles_are_matched_by_name() {
        let shared = Shared::new();
        let (pc, mac) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
        let id = record(&pc, &shared.0);
        let elena = pc.speakers(id).unwrap()[1].id;
        pc.rename_speaker(elena, None).unwrap();
        pc.create_profile_from_speaker(elena, "Elena").unwrap();
        flush(&pc, &md);

        // No such profile on the Mac: the name still shows.
        scan(&mac, &shared.0);
        let m = only(&mac);
        assert_eq!(m.speakers, ["Elena"]);
        let sp = &mac.speakers(m.id).unwrap()[1];
        assert_eq!((sp.name.as_deref(), sp.profile_id), (Some("Elena"), None));
        // Saved back unchanged, so the PC keeps its link.
        assert_eq!(
            content_hash(&mac.meeting_file(m.id).unwrap().unwrap()),
            content_hash(&pc.meeting_file(id).unwrap().unwrap())
        );
    }

    #[test]
    fn folder_names_are_dated_and_unique() {
        let a = folder_name(1_790_000_000_000, &new_uid());
        let b = folder_name(1_790_000_000_000, &new_uid());
        assert_ne!(a, b);
        assert!(a.starts_with("2026-") || a.starts_with("2027-"), "{a}");
        assert!(!a.contains(':') && !a.contains('/'));
    }
}
