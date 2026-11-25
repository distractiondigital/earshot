//! SQLite storage: connection setup and schema migrations.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};

/// Ordered list of schema migrations. Append only; never edit a released migration.
static MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("../migrations/0001_init.sql")),
        M::up(include_str!("../migrations/0002_recording.sql")),
        M::up(include_str!("../migrations/0003_source_spec.sql")),
        M::up(include_str!("../migrations/0004_jobs.sql")),
    ])
});

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRow {
    pub id: i64,
    pub title: String,
    pub started_at: i64,
    pub duration_ms: i64,
    pub status: String,
    pub error: Option<String>,
    pub remote_speaker_count_hint: Option<i64>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRow {
    pub id: i64,
    pub meeting_id: i64,
    pub kind: String,
    pub stage: Option<String>,
    pub progress: f64,
    pub status: String,
    pub error: Option<String>,
    pub options: Option<String>,
}

/// One utterance to store: a run of words on one track without a long pause.
#[derive(Debug, Clone, Default)]
pub struct NewUtterance {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub words: Vec<NewWord>,
}

#[derive(Debug, Clone, Default)]
pub struct NewWord {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub prob: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtteranceRow {
    pub id: i64,
    pub track_kind: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub words: Vec<WordRow>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub prob: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    pub id: i64,
    pub meeting_id: i64,
    pub kind: String,
    pub path: String,
    pub sample_rate: i64,
    pub channels: i64,
    pub duration_ms: i64,
    pub finalized: bool,
    pub source: Option<String>,
    /// JSON `SystemSource` for the meeting track.
    pub source_spec: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, DbError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        MIGRATIONS.to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means a panic happened mid-query; SQLite itself stays consistent.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn schema_version(&self) -> Result<u32, DbError> {
        let v: u32 = self
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- Meetings & tracks ----

    pub fn create_meeting(&self, started_at: i64, title: &str) -> Result<i64, DbError> {
        let c = self.conn();
        c.execute(
            "INSERT INTO meetings (title, started_at, status, created_at, updated_at)
             VALUES (?1, ?2, 'recording', ?2, ?2)",
            params![title, started_at],
        )?;
        Ok(c.last_insert_rowid())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_track(
        &self,
        meeting_id: i64,
        kind: &str,
        path: &str,
        sample_rate: u32,
        channels: u16,
        source: &str,
        source_spec: Option<&str>,
    ) -> Result<i64, DbError> {
        let c = self.conn();
        c.execute(
            "INSERT INTO tracks (meeting_id, kind, path, sample_rate, channels, source, source_spec)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![meeting_id, kind, path, sample_rate, channels, source, source_spec],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn finalize_track(&self, track_id: i64, duration_ms: i64) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE tracks SET duration_ms = ?2, finalized = 1 WHERE id = ?1",
            params![track_id, duration_ms],
        )?;
        Ok(())
    }

    pub fn set_meeting_status(
        &self,
        id: i64,
        status: &str,
        duration_ms: Option<i64>,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE meetings SET status = ?2, duration_ms = COALESCE(?3, duration_ms), error = ?4,
             updated_at = ?5 WHERE id = ?1",
            params![id, status, duration_ms, error, now_ms()],
        )?;
        Ok(())
    }

    pub fn update_meeting_details(
        &self,
        id: i64,
        title: &str,
        speaker_hint: Option<i64>,
        tags: &[String],
    ) -> Result<(), DbError> {
        let tags = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
        self.conn().execute(
            "UPDATE meetings SET title = ?2, remote_speaker_count_hint = ?3, tags = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, title, speaker_hint, tags, now_ms()],
        )?;
        Ok(())
    }

    pub fn meeting(&self, id: i64) -> Result<Option<MeetingRow>, DbError> {
        Ok(self.list_meetings()?.into_iter().find(|m| m.id == id))
    }

    pub fn latest_meeting_with_status(&self, status: &str) -> Result<Option<MeetingRow>, DbError> {
        Ok(self
            .list_meetings()?
            .into_iter()
            .find(|m| m.status == status))
    }

    /// Marks a track as being written again (continuing an interrupted meeting).
    pub fn reopen_track(&self, track_id: i64) -> Result<(), DbError> {
        self.conn()
            .execute("UPDATE tracks SET finalized = 0 WHERE id = ?1", [track_id])?;
        Ok(())
    }

    // ---- Processing jobs ----

    /// Queues a job unless one is already queued or running for this meeting. Returns its id.
    pub fn enqueue_job(
        &self,
        meeting_id: i64,
        kind: &str,
        options: Option<&str>,
    ) -> Result<i64, DbError> {
        let c = self.conn();
        if let Some(id) = c
            .query_row(
                "SELECT id FROM jobs WHERE meeting_id = ?1 AND kind = ?2 AND status IN ('queued', 'running')",
                params![meeting_id, kind],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        c.execute(
            "INSERT INTO jobs (meeting_id, kind, status, progress, options, created_at, updated_at)
             VALUES (?1, ?2, 'queued', 0, ?3, ?4, ?4)",
            params![meeting_id, kind, options, now_ms()],
        )?;
        Ok(c.last_insert_rowid())
    }

    fn job_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
        Ok(JobRow {
            id: r.get(0)?,
            meeting_id: r.get(1)?,
            kind: r.get(2)?,
            stage: r.get(3)?,
            progress: r.get(4)?,
            status: r.get(5)?,
            error: r.get(6)?,
            options: r.get(7)?,
        })
    }

    pub fn next_queued_job(&self) -> Result<Option<JobRow>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, meeting_id, kind, stage, progress, status, error, options FROM jobs
                 WHERE status = 'queued' ORDER BY created_at, id LIMIT 1",
                [],
                Self::job_from_row,
            )
            .optional()?)
    }

    /// Latest job of each meeting (for status display).
    pub fn latest_jobs(&self) -> Result<Vec<JobRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, meeting_id, kind, stage, progress, status, error, options FROM jobs j
             WHERE id = (SELECT max(id) FROM jobs WHERE meeting_id = j.meeting_id)",
        )?;
        let rows = stmt.query_map([], Self::job_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn update_job(
        &self,
        id: i64,
        status: &str,
        stage: Option<&str>,
        progress: f64,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE jobs SET status = ?2, stage = ?3, progress = ?4, error = ?5, updated_at = ?6 WHERE id = ?1",
            params![id, status, stage, progress, error, now_ms()],
        )?;
        Ok(())
    }

    /// Cancels a meeting's queued (not yet running) jobs. Returns how many.
    pub fn cancel_queued_jobs(&self, meeting_id: i64) -> Result<usize, DbError> {
        Ok(self.conn().execute(
            "UPDATE jobs SET status = 'cancelled', updated_at = ?2 WHERE meeting_id = ?1 AND status = 'queued'",
            params![meeting_id, now_ms()],
        )?)
    }

    /// Jobs and meetings left running by a crash are queued again.
    pub fn requeue_interrupted_jobs(&self) -> Result<usize, DbError> {
        let c = self.conn();
        let n = c.execute(
            "UPDATE jobs SET status = 'queued', stage = NULL, progress = 0, updated_at = ?1 WHERE status = 'running'",
            [now_ms()],
        )?;
        c.execute(
            "UPDATE meetings SET status = 'queued' WHERE status = 'processing'",
            [],
        )?;
        Ok(n)
    }

    // ---- Transcripts ----

    /// Replaces one track's transcript (idempotent re-runs). Words cascade; FTS stays in sync.
    pub fn replace_track_transcript(
        &self,
        meeting_id: i64,
        track_kind: &str,
        utterances: &[NewUtterance],
    ) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM utterances WHERE meeting_id = ?1 AND track_kind = ?2",
            params![meeting_id, track_kind],
        )?;
        {
            let mut ins_u = tx.prepare(
                "INSERT INTO utterances (meeting_id, track_kind, start_ms, end_ms, text) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut ins_w = tx.prepare(
                "INSERT INTO words (utterance_id, start_ms, end_ms, text, prob) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for u in utterances {
                ins_u.execute(params![
                    meeting_id, track_kind, u.start_ms, u.end_ms, u.text
                ])?;
                let uid = tx.last_insert_rowid();
                for w in &u.words {
                    ins_w.execute(params![
                        uid,
                        w.start_ms,
                        w.end_ms,
                        w.text,
                        f64::from(w.prob)
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_meeting_language(&self, id: i64, language: &str) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE meetings SET language = ?2 WHERE id = ?1",
            params![id, language],
        )?;
        Ok(())
    }

    /// All utterances of a meeting in time order, with their words.
    pub fn transcript(&self, meeting_id: i64) -> Result<Vec<UtteranceRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, track_kind, start_ms, end_ms, text FROM utterances WHERE meeting_id = ?1
             ORDER BY start_ms, id",
        )?;
        let mut out: Vec<UtteranceRow> = stmt
            .query_map([meeting_id], |r| {
                Ok(UtteranceRow {
                    id: r.get(0)?,
                    track_kind: r.get(1)?,
                    start_ms: r.get(2)?,
                    end_ms: r.get(3)?,
                    text: r.get(4)?,
                    words: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut wstmt = c.prepare(
            "SELECT start_ms, end_ms, text, prob FROM words WHERE utterance_id = ?1 ORDER BY start_ms, id",
        )?;
        for u in &mut out {
            u.words = wstmt
                .query_map([u.id], |r| {
                    Ok(WordRow {
                        start_ms: r.get(0)?,
                        end_ms: r.get(1)?,
                        text: r.get(2)?,
                        prob: r.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
        }
        Ok(out)
    }

    /// Meetings left in `recording` by a crash become `incomplete` (offered for recovery).
    pub fn mark_interrupted(&self) -> Result<usize, DbError> {
        Ok(self.conn().execute(
            "UPDATE meetings SET status = 'incomplete', updated_at = ?1 WHERE status = 'recording'",
            [now_ms()],
        )?)
    }

    pub fn list_meetings(&self) -> Result<Vec<MeetingRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, title, started_at, duration_ms, status, error, remote_speaker_count_hint, tags
             FROM meetings ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let tags: String = r.get(7)?;
            Ok(MeetingRow {
                id: r.get(0)?,
                title: r.get(1)?,
                started_at: r.get(2)?,
                duration_ms: r.get(3)?,
                status: r.get(4)?,
                error: r.get(5)?,
                remote_speaker_count_hint: r.get(6)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn meeting_status(&self, id: i64) -> Result<Option<String>, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT status FROM meetings WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn tracks(&self, meeting_id: i64) -> Result<Vec<TrackRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, meeting_id, kind, path, sample_rate, channels, duration_ms, finalized, source,
                    source_spec
             FROM tracks WHERE meeting_id = ?1 ORDER BY kind",
        )?;
        let rows = stmt.query_map([meeting_id], |r| {
            Ok(TrackRow {
                id: r.get(0)?,
                meeting_id: r.get(1)?,
                kind: r.get(2)?,
                path: r.get(3)?,
                sample_rate: r.get(4)?,
                channels: r.get(5)?,
                duration_ms: r.get(6)?,
                finalized: r.get::<_, i64>(7)? != 0,
                source: r.get(8)?,
                source_spec: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        MIGRATIONS.validate().unwrap();
    }

    #[test]
    fn migrates_to_latest() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), 4);
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("earshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        Db::open(&path)
            .unwrap()
            .set_setting("theme", "dark")
            .unwrap();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get_setting("theme").unwrap().as_deref(), Some("dark"));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("theme").unwrap(), None);
        db.set_setting("theme", "dark").unwrap();
        db.set_setting("theme", "light").unwrap();
        assert_eq!(db.get_setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn fts_tracks_utterance_changes() {
        let db = Db::open_in_memory().unwrap();
        let c = db.conn();
        c.execute(
            "INSERT INTO meetings (id, started_at, created_at, updated_at) VALUES (1, 0, 0, 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO utterances (id, meeting_id, track_kind, start_ms, end_ms, text)
             VALUES (1, 1, 'system', 0, 1000, 'quarterly roadmap review')",
            [],
        )
        .unwrap();
        let hits = |q: &str| -> i64 {
            c.query_row(
                "SELECT count(*) FROM utterances_fts WHERE utterances_fts MATCH ?1",
                [q],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(hits("roadmap"), 1);
        c.execute(
            "UPDATE utterances SET text = 'budget planning' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(hits("roadmap"), 0);
        assert_eq!(hits("budget"), 1);
        c.execute("DELETE FROM meetings WHERE id = 1", []).unwrap();
        assert_eq!(hits("budget"), 0);
    }

    #[test]
    fn meeting_lifecycle_and_interrupt() {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_meeting(1_000, "").unwrap();
        let t = db
            .add_track(id, "mic", "/x/mic.flac", 48_000, 1, "Mic", None)
            .unwrap();
        assert_eq!(db.mark_interrupted().unwrap(), 1);
        assert_eq!(
            db.meeting_status(id).unwrap().as_deref(),
            Some("incomplete")
        );
        db.finalize_track(t, 5_000).unwrap();
        db.set_meeting_status(id, "queued", Some(5_000), None)
            .unwrap();
        db.update_meeting_details(id, "Standup", Some(3), &["team".into()])
            .unwrap();
        let m = &db.list_meetings().unwrap()[0];
        assert_eq!(
            (m.title.as_str(), m.duration_ms, m.status.as_str()),
            ("Standup", 5_000, "queued")
        );
        assert_eq!(m.tags, vec!["team".to_string()]);
        let tr = &db.tracks(id).unwrap()[0];
        assert!(tr.finalized && tr.duration_ms == 5_000 && tr.source.as_deref() == Some("Mic"));
    }

    #[test]
    fn jobs_and_transcripts() {
        let db = Db::open_in_memory().unwrap();
        let m = db.create_meeting(0, "").unwrap();
        let j = db.enqueue_job(m, "transcribe", None).unwrap();
        assert_eq!(
            db.enqueue_job(m, "transcribe", None).unwrap(),
            j,
            "no duplicate queued job"
        );
        assert_eq!(db.next_queued_job().unwrap().unwrap().id, j);
        db.update_job(j, "running", Some("transcribe"), 50.0, None)
            .unwrap();
        assert!(db.next_queued_job().unwrap().is_none());
        assert_eq!(db.requeue_interrupted_jobs().unwrap(), 1);
        assert_eq!(db.next_queued_job().unwrap().unwrap().status, "queued");

        let utt = |s: i64, text: &str| NewUtterance {
            start_ms: s,
            end_ms: s + 500,
            text: text.into(),
            words: vec![NewWord {
                start_ms: s,
                end_ms: s + 500,
                text: format!(" {text}"),
                prob: 0.9,
            }],
        };
        db.replace_track_transcript(m, "system", &[utt(2000, "hello")])
            .unwrap();
        db.replace_track_transcript(m, "mic", &[utt(1000, "hi")])
            .unwrap();
        db.replace_track_transcript(m, "system", &[utt(3000, "world")])
            .unwrap();
        let t = db.transcript(m).unwrap();
        assert_eq!(
            t.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            ["hi", "world"]
        );
        assert_eq!(t[1].words[0].text, " world");
        let fts: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM utterances_fts WHERE utterances_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0, "replaced text is gone from search");
    }
}
