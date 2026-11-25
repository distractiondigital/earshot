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
    /// Detected or chosen language (ISO 639-1).
    pub language: Option<String>,
    /// Remote speakers by name (or "Speaker N"), most talkative first, for the list's chips.
    pub speakers: Vec<String>,
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
    /// Index into the speakers passed to [`Db::save_transcript`].
    pub speaker: Option<usize>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub words: Vec<NewWord>,
}

#[derive(Debug, Clone)]
pub struct NewSpeaker {
    /// "Me" or "Speaker N".
    pub label: String,
    pub is_me: bool,
    /// Name carried over from before a re-run.
    pub name: Option<String>,
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
    pub speaker_id: Option<i64>,
    /// "Me" or "Speaker N"; `None` when the speaker isn't known.
    pub speaker_label: Option<String>,
    /// Name given by the user (M4).
    pub speaker_name: Option<String>,
    pub is_me: bool,
    /// The text was corrected by hand; word timings may no longer match it.
    pub edited: bool,
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

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerRow {
    pub id: i64,
    /// "Me" or "Speaker N".
    pub label: String,
    /// Name given by the user.
    pub name: Option<String>,
    pub is_me: bool,
    pub talk_ms: i64,
    pub utterances: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub meeting_id: i64,
    pub utterance_id: i64,
    pub start_ms: i64,
    /// Matching text with matches between \u{1} and \u{2}.
    pub snippet: String,
}

/// Turns what the user typed into a safe FTS5 query: every word quoted (so punctuation and
/// operators are plain text) and matched as a prefix. `None` when there's nothing to search for.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '-')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .take(12)
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Invalid(&'static str),
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

    /// Replaces a meeting's speakers and its whole transcript in one transaction.
    pub fn save_transcript(
        &self,
        meeting_id: i64,
        speakers: &[NewSpeaker],
        tracks: &[(&str, &[NewUtterance])],
    ) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM utterances WHERE meeting_id = ?1", [meeting_id])?;
        tx.execute("DELETE FROM speakers WHERE meeting_id = ?1", [meeting_id])?;
        let mut ids = Vec::with_capacity(speakers.len());
        {
            let mut ins_s = tx.prepare(
                "INSERT INTO speakers (meeting_id, label, is_me, display_name) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for sp in speakers {
                ins_s.execute(params![meeting_id, sp.label, sp.is_me, sp.name])?;
                ids.push(tx.last_insert_rowid());
            }
            let mut ins_u = tx.prepare(
                "INSERT INTO utterances (meeting_id, speaker_id, track_kind, start_ms, end_ms, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut ins_w = tx.prepare(
                "INSERT INTO words (utterance_id, start_ms, end_ms, text, prob) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (kind, utterances) in tracks {
                for u in *utterances {
                    let speaker_id = u.speaker.and_then(|i| ids.get(i).copied());
                    ins_u.execute(params![
                        meeting_id, speaker_id, kind, u.start_ms, u.end_ms, u.text
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

    /// A meeting's speakers with how long each one talks.
    pub fn speakers(&self, meeting_id: i64) -> Result<Vec<SpeakerRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT s.id, s.label, s.display_name, s.is_me,
                    coalesce(sum(u.end_ms - u.start_ms), 0), count(u.id)
             FROM speakers s LEFT JOIN utterances u ON u.speaker_id = s.id
             WHERE s.meeting_id = ?1
             GROUP BY s.id
             ORDER BY s.is_me DESC, s.id",
        )?;
        let rows = stmt
            .query_map([meeting_id], |r| {
                Ok(SpeakerRow {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    name: r.get(2)?,
                    is_me: r.get::<_, i64>(3)? != 0,
                    talk_ms: r.get(4)?,
                    utterances: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Sets (or with `None`, clears) the name shown for a speaker.
    pub fn rename_speaker(&self, speaker_id: i64, name: Option<&str>) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE speakers SET display_name = ?2 WHERE id = ?1",
            params![speaker_id, name],
        )?;
        Ok(())
    }

    /// Moves every utterance of `from` to `into` and removes `from`. Both must belong to the
    /// same meeting, and the mic speaker ("Me") can't be merged away.
    pub fn merge_speakers(&self, from: i64, into: i64) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let ok: bool = tx.query_row(
            "SELECT count(*) = 2 AND sum(is_me AND id = ?1) = 0 AND count(DISTINCT meeting_id) = 1
             FROM speakers WHERE id IN (?1, ?2)",
            params![from, into],
            |r| r.get(0),
        )?;
        if !ok || from == into {
            return Err(DbError::Invalid("those speakers can't be merged"));
        }
        tx.execute(
            "UPDATE utterances SET speaker_id = ?2 WHERE speaker_id = ?1",
            params![from, into],
        )?;
        tx.execute("DELETE FROM speakers WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(())
    }

    /// Gives one utterance to another speaker of the same meeting.
    pub fn set_utterance_speaker(&self, utterance_id: i64, speaker_id: i64) -> Result<(), DbError> {
        let n = self.conn().execute(
            "UPDATE utterances SET speaker_id = ?2
             WHERE id = ?1
               AND meeting_id = (SELECT meeting_id FROM speakers WHERE id = ?2)",
            params![utterance_id, speaker_id],
        )?;
        if n == 0 {
            return Err(DbError::Invalid("that speaker isn't in this meeting"));
        }
        Ok(())
    }

    /// Replaces an utterance's text (a hand correction); search follows via the FTS trigger.
    pub fn set_utterance_text(&self, utterance_id: i64, text: &str) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE utterances SET text = ?2, edited = 1 WHERE id = ?1",
            params![utterance_id, text],
        )?;
        Ok(())
    }

    /// Every stored word of a meeting per track, in time order (for re-running speaker separation
    /// without transcribing again).
    pub fn track_words(&self, meeting_id: i64) -> Result<Vec<(String, Vec<NewWord>)>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT u.track_kind, w.start_ms, w.end_ms, w.text, w.prob
             FROM words w JOIN utterances u ON u.id = w.utterance_id
             WHERE u.meeting_id = ?1
             ORDER BY u.track_kind, w.start_ms, w.id",
        )?;
        let mut out: Vec<(String, Vec<NewWord>)> = Vec::new();
        let rows = stmt.query_map([meeting_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                NewWord {
                    start_ms: r.get(1)?,
                    end_ms: r.get(2)?,
                    text: r.get(3)?,
                    prob: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0) as f32,
                },
            ))
        })?;
        for row in rows {
            let (kind, w) = row?;
            match out.last_mut() {
                Some((k, ws)) if *k == kind => ws.push(w),
                _ => out.push((kind, vec![w])),
            }
        }
        Ok(out)
    }

    /// Remote speakers the user named, with when they spoke (to carry names over a re-run).
    pub fn named_speakers(
        &self,
        meeting_id: i64,
    ) -> Result<Vec<crate::speakers::OldSpeaker>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT s.id, s.display_name, u.start_ms, u.end_ms
             FROM speakers s JOIN utterances u ON u.speaker_id = s.id
             WHERE s.meeting_id = ?1 AND s.is_me = 0 AND s.display_name IS NOT NULL
             ORDER BY s.id, u.start_ms",
        )?;
        let mut out: Vec<(i64, crate::speakers::OldSpeaker)> = Vec::new();
        let rows = stmt.query_map([meeting_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, name, s, e) = row?;
            match out.last_mut() {
                Some((last, o)) if *last == id => o.spans.push((s, e)),
                _ => out.push((
                    id,
                    crate::speakers::OldSpeaker {
                        name,
                        spans: vec![(s, e)],
                    },
                )),
            }
        }
        Ok(out.into_iter().map(|(_, o)| o).collect())
    }

    /// Full-text search over every transcript, newest meetings first. Each word of `query` must
    /// appear (as a word prefix). Snippets mark matches with \u{1} … \u{2}.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, DbError> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT u.meeting_id, u.id, u.start_ms,
                    snippet(utterances_fts, 0, char(1), char(2), '…', 14)
             FROM utterances_fts
             JOIN utterances u ON u.id = utterances_fts.rowid
             JOIN meetings m ON m.id = u.meeting_id
             WHERE utterances_fts MATCH ?1
             ORDER BY m.started_at DESC, u.start_ms
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![fts, limit as i64], |r| {
                Ok(SearchHit {
                    meeting_id: r.get(0)?,
                    utterance_id: r.get(1)?,
                    start_ms: r.get(2)?,
                    snippet: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Deletes a meeting and everything attached to it; returns its track files to remove.
    pub fn delete_meeting(&self, id: i64) -> Result<Vec<String>, DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let paths = {
            let mut stmt = tx.prepare("SELECT path FROM tracks WHERE meeting_id = ?1")?;
            stmt.query_map([id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        // Utterances, words, speakers, tracks and jobs go with it (ON DELETE CASCADE).
        tx.execute("DELETE FROM meetings WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(paths)
    }

    /// All utterances of a meeting in time order, with their words.
    pub fn transcript(&self, meeting_id: i64) -> Result<Vec<UtteranceRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT u.id, u.track_kind, u.start_ms, u.end_ms, u.text,
                    u.speaker_id, s.label, s.display_name, coalesce(s.is_me, 0), u.edited
             FROM utterances u LEFT JOIN speakers s ON s.id = u.speaker_id
             WHERE u.meeting_id = ?1
             ORDER BY u.start_ms, u.id",
        )?;
        let mut out: Vec<UtteranceRow> = stmt
            .query_map([meeting_id], |r| {
                Ok(UtteranceRow {
                    id: r.get(0)?,
                    track_kind: r.get(1)?,
                    start_ms: r.get(2)?,
                    end_ms: r.get(3)?,
                    text: r.get(4)?,
                    speaker_id: r.get(5)?,
                    speaker_label: r.get(6)?,
                    speaker_name: r.get(7)?,
                    is_me: r.get::<_, i64>(8)? != 0,
                    edited: r.get::<_, i64>(9)? != 0,
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
            "SELECT m.id, m.title, m.started_at, m.duration_ms, m.status, m.error,
                    m.remote_speaker_count_hint, m.tags, m.language,
                    (SELECT json_group_array(name) FROM (
                        SELECT coalesce(s.display_name, s.label) AS name
                        FROM speakers s LEFT JOIN utterances u ON u.speaker_id = s.id
                        WHERE s.meeting_id = m.id AND s.is_me = 0
                        GROUP BY s.id
                        ORDER BY coalesce(sum(u.end_ms - u.start_ms), 0) DESC))
             FROM meetings m ORDER BY m.started_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let tags: String = r.get(7)?;
            let speakers: Option<String> = r.get(9)?;
            Ok(MeetingRow {
                id: r.get(0)?,
                title: r.get(1)?,
                started_at: r.get(2)?,
                duration_ms: r.get(3)?,
                status: r.get(4)?,
                error: r.get(5)?,
                remote_speaker_count_hint: r.get(6)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
                language: r.get(8)?,
                speakers: speakers
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
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
    fn speaker_editing() {
        let db = Db::open_in_memory().unwrap();
        let m = db.create_meeting(1_000, "a").unwrap();
        let other = db.create_meeting(2_000, "b").unwrap();
        let word = |s: i64, t: &str| NewWord {
            start_ms: s,
            end_ms: s + 400,
            text: format!(" {t}"),
            prob: 0.9,
        };
        let utt = |speaker: usize, s: i64, t: &str| NewUtterance {
            speaker: Some(speaker),
            start_ms: s,
            end_ms: s + 400,
            text: t.into(),
            words: vec![word(s, t)],
        };
        let speakers = [
            NewSpeaker {
                label: "Speaker 1".into(),
                is_me: false,
                name: None,
            },
            NewSpeaker {
                label: "Speaker 2".into(),
                is_me: false,
                name: None,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
            },
        ];
        db.save_transcript(
            m,
            &speakers,
            &[
                (
                    "system",
                    &[
                        utt(0, 0, "alpha"),
                        utt(1, 1000, "bravo"),
                        utt(0, 2000, "charlie"),
                    ],
                ),
                ("mic", &[utt(2, 3000, "delta")]),
            ],
        )
        .unwrap();
        db.save_transcript(
            other,
            &speakers[..1],
            &[("system", &[utt(0, 0, "elsewhere")])],
        )
        .unwrap();
        let sp = db.speakers(m).unwrap();
        assert_eq!(
            sp.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
            ["Me", "Speaker 1", "Speaker 2"]
        );
        let (me, s1, s2) = (sp[0].id, sp[1].id, sp[2].id);
        assert_eq!((sp[1].utterances, sp[1].talk_ms), (2, 800));
        let foreign = db.speakers(other).unwrap()[0].id;

        db.rename_speaker(s1, Some("Dana")).unwrap();
        assert_eq!(
            db.transcript(m).unwrap()[0].speaker_name.as_deref(),
            Some("Dana")
        );

        // The mic speaker can't be merged away, and speakers of different meetings can't merge.
        assert!(db.merge_speakers(me, s1).is_err());
        assert!(db.merge_speakers(s2, foreign).is_err());
        db.merge_speakers(s2, s1).unwrap();
        let sp = db.speakers(m).unwrap();
        assert_eq!(sp.len(), 2);
        assert_eq!(sp[1].utterances, 3);

        // Reassigning one line, only within the meeting.
        let t = db.transcript(m).unwrap();
        let bravo = t.iter().find(|u| u.text == "bravo").unwrap().id;
        db.set_utterance_speaker(bravo, me).unwrap();
        assert!(db.set_utterance_speaker(bravo, foreign).is_err());
        assert!(
            db.transcript(m)
                .unwrap()
                .iter()
                .find(|u| u.id == bravo)
                .unwrap()
                .is_me
        );

        // Edited text is marked and searchable.
        db.set_utterance_text(bravo, "bravo corrected").unwrap();
        let u = db
            .transcript(m)
            .unwrap()
            .into_iter()
            .find(|u| u.id == bravo)
            .unwrap();
        assert!(u.edited);
        let hits: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM utterances_fts WHERE utterances_fts MATCH 'corrected'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn stored_words_and_named_speakers_for_reruns() {
        let db = Db::open_in_memory().unwrap();
        let m = db.create_meeting(1_000, "a").unwrap();
        let u = |speaker: usize, s: i64, words: &[&str]| NewUtterance {
            speaker: Some(speaker),
            start_ms: s,
            end_ms: s + 300 * words.len() as i64,
            text: words.join(" "),
            words: words
                .iter()
                .enumerate()
                .map(|(i, w)| NewWord {
                    start_ms: s + 300 * i as i64,
                    end_ms: s + 300 * (i as i64 + 1),
                    text: format!(" {w}"),
                    prob: 0.5,
                })
                .collect(),
        };
        let speakers = [
            NewSpeaker {
                label: "Speaker 1".into(),
                is_me: false,
                name: Some("Dana".into()),
            },
            NewSpeaker {
                label: "Speaker 2".into(),
                is_me: false,
                name: None,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
            },
        ];
        db.save_transcript(
            m,
            &speakers,
            &[
                (
                    "system",
                    &[u(0, 0, &["a", "b"]), u(1, 5000, &["c"]), u(0, 9000, &["d"])],
                ),
                ("mic", &[u(2, 2000, &["e", "f"])]),
            ],
        )
        .unwrap();
        let words = db.track_words(m).unwrap();
        let summary: Vec<(String, Vec<String>)> = words
            .iter()
            .map(|(k, ws)| {
                (
                    k.clone(),
                    ws.iter().map(|w| w.text.trim().to_owned()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("mic".to_owned(), vec!["e".to_owned(), "f".to_owned()]),
                (
                    "system".to_owned(),
                    vec!["a".into(), "b".into(), "c".into(), "d".into()]
                ),
            ]
        );
        let named = db.named_speakers(m).unwrap();
        assert_eq!(named.len(), 1, "unnamed and 'Me' speakers are left out");
        assert_eq!(named[0].name, "Dana");
        assert_eq!(named[0].spans, [(0, 600), (9000, 9300)]);
    }

    #[test]
    fn search_and_delete() {
        let db = Db::open_in_memory().unwrap();
        let a = db.create_meeting(1_000, "older").unwrap();
        let b = db.create_meeting(2_000, "newer").unwrap();
        let u = |s: i64, t: &str| NewUtterance {
            speaker: None,
            start_ms: s,
            end_ms: s + 1000,
            text: t.into(),
            words: Vec::new(),
        };
        db.save_transcript(
            a,
            &[],
            &[(
                "system",
                &[u(5_000, "The budget review moved to Thursday.")],
            )],
        )
        .unwrap();
        db.save_transcript(
            b,
            &[],
            &[(
                "system",
                &[u(9_000, "Budgets are due; budget-wise we're fine.")],
            )],
        )
        .unwrap();
        db.add_track(
            a,
            "system",
            "C:/rec/meeting-a/system.flac",
            48_000,
            2,
            "Default system audio",
            None,
        )
        .unwrap();

        let hits = db.search("budg", 50).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.meeting_id).collect::<Vec<_>>(),
            [b, a],
            "newest first"
        );
        assert!(
            hits[1].snippet.contains("\u{1}budget\u{2}"),
            "{:?}",
            hits[1].snippet
        );
        assert_eq!(
            db.search("budget thursday", 50).unwrap().len(),
            1,
            "all words must match"
        );
        // Operators and quotes are plain text, not query syntax.
        assert!(db.search("\" OR * NEAR( -", 50).is_ok());
        assert!(db.search("   ", 50).unwrap().is_empty());

        assert_eq!(
            db.delete_meeting(a).unwrap(),
            ["C:/rec/meeting-a/system.flac"]
        );
        assert!(db.meeting(a).unwrap().is_none());
        assert!(
            db.search("thursday", 50).unwrap().is_empty(),
            "search index follows deletes"
        );
        assert_eq!(db.search("budget", 50).unwrap().len(), 1);
    }

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
            speaker: None,
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

        // The final save replaces speakers and both tracks together.
        let speakers = [
            NewSpeaker {
                label: "Speaker 1".into(),
                is_me: false,
                name: None,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
            },
        ];
        let mut sys = utt(500, "welcome");
        sys.speaker = Some(0);
        let mut mine = utt(900, "thanks");
        mine.speaker = Some(1);
        let unknown = utt(4000, "mystery");
        db.save_transcript(
            m,
            &speakers,
            &[("system", &[sys, unknown]), ("mic", &[mine])],
        )
        .unwrap();
        let t = db.transcript(m).unwrap();
        assert_eq!(
            t.iter()
                .map(|u| (u.text.as_str(), u.speaker_label.as_deref(), u.is_me))
                .collect::<Vec<_>>(),
            [
                ("welcome", Some("Speaker 1"), false),
                ("thanks", Some("Me"), true),
                ("mystery", None, false)
            ]
        );
        // Saving again replaces speakers rather than adding more.
        db.save_transcript(m, &speakers, &[]).unwrap();
        let n: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM speakers WHERE meeting_id = ?1",
                [m],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        assert!(db.transcript(m).unwrap().is_empty());
    }
}
