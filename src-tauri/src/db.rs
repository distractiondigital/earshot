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

    pub fn add_track(
        &self,
        meeting_id: i64,
        kind: &str,
        path: &str,
        sample_rate: u32,
        channels: u16,
        source: &str,
    ) -> Result<i64, DbError> {
        let c = self.conn();
        c.execute(
            "INSERT INTO tracks (meeting_id, kind, path, sample_rate, channels, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![meeting_id, kind, path, sample_rate, channels, source],
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
            "SELECT id, meeting_id, kind, path, sample_rate, channels, duration_ms, finalized, source
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
        assert_eq!(db.schema_version().unwrap(), 2);
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
            .add_track(id, "mic", "/x/mic.flac", 48_000, 1, "Mic")
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
}
