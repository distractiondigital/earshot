//! SQLite storage: connection setup and schema migrations.

use std::collections::HashMap;
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
        M::up(include_str!("../migrations/0005_voice_profiles.sql")),
        M::up(include_str!("../migrations/0006_summaries.sql")),
        M::up(include_str!("../migrations/0007_library.sql")),
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
    /// Recorded on another computer (read in from its folder).
    pub elsewhere: bool,
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
    /// Voice embedding from speaker separation, with the model that made it.
    pub embedding: Option<(String, Vec<f32>)>,
    /// Clean speech that went into the embedding.
    pub speech_ms: u64,
    /// Linked voice profile; `auto_matched` when the app suggested it and the user hasn't
    /// confirmed it yet.
    pub profile_id: Option<i64>,
    pub auto_matched: bool,
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
    /// Name given by the user (M4), or the linked voice profile's name.
    pub speaker_name: Option<String>,
    /// `speaker_name` is a voice-match suggestion the user hasn't confirmed.
    pub speaker_suggested: bool,
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
    /// Linked voice profile and its name.
    pub profile_id: Option<i64>,
    pub profile_name: Option<String>,
    /// The profile was suggested by voice matching and isn't confirmed yet.
    pub auto_matched: bool,
    /// A voice embedding is stored, so the speaker can be saved as a profile.
    pub has_voice: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRow {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    /// Meetings the profile appears in, and when it was last heard.
    pub meetings: i64,
    pub last_seen: Option<i64>,
    /// Samples for each voice model.
    pub samples: Vec<(String, i64)>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMeeting {
    pub meeting_id: i64,
    pub title: String,
    pub started_at: i64,
    pub auto_matched: bool,
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
    device: std::sync::OnceLock<String>,
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
        let db = Self {
            conn: Mutex::new(conn),
            device: std::sync::OnceLock::new(),
        };
        db.ensure_builtin_templates()?;
        Ok(db)
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
            "INSERT INTO meetings (title, started_at, status, created_at, updated_at, uid)
             VALUES (?1, ?2, 'recording', ?2, ?2, ?3)",
            params![title, started_at, crate::library::new_uid()],
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
            "UPDATE meetings SET title_generated = (title_generated AND title = ?2), title = ?2,
                 remote_speaker_count_hint = ?3, tags = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, title, speaker_hint, tags, now_ms()],
        )?;
        Ok(())
    }

    pub fn meeting(&self, id: i64) -> Result<Option<MeetingRow>, DbError> {
        Ok(self.list_meetings()?.into_iter().find(|m| m.id == id))
    }

    /// Marks a track as being written again (continuing an interrupted meeting).
    /// Records the source a track switched to mid-meeting.
    pub fn set_track_source(
        &self,
        track_id: i64,
        source: &str,
        source_spec: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE tracks SET source = ?2, source_spec = ?3 WHERE id = ?1",
            params![track_id, source, source_spec],
        )?;
        Ok(())
    }

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

    /// Latest job of a kind for each meeting (for status display).
    pub fn latest_jobs(&self, kind: &str) -> Result<Vec<JobRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, meeting_id, kind, stage, progress, status, error, options FROM jobs j
             WHERE id = (SELECT max(id) FROM jobs WHERE meeting_id = j.meeting_id AND kind = ?1)",
        )?;
        let rows = stmt.query_map([kind], Self::job_from_row)?;
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
    pub fn cancel_queued_jobs(&self, meeting_id: i64, kind: &str) -> Result<usize, DbError> {
        Ok(self.conn().execute(
            "UPDATE jobs SET status = 'cancelled', updated_at = ?2
             WHERE meeting_id = ?1 AND kind = ?3 AND status = 'queued'",
            params![meeting_id, now_ms(), kind],
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
            "UPDATE meetings SET status = 'queued' WHERE status = 'processing' AND device IS NULL",
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
                "INSERT INTO speakers (meeting_id, label, is_me, display_name, embedding,
                                       embedding_model, speech_ms, profile_id, auto_matched)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for sp in speakers {
                let (model, blob) = match &sp.embedding {
                    Some((m, v)) => (Some(m.as_str()), Some(crate::profiles::to_blob(v))),
                    None => (None, None),
                };
                ins_s.execute(params![
                    meeting_id,
                    sp.label,
                    sp.is_me,
                    sp.name,
                    blob,
                    model,
                    sp.speech_ms as i64,
                    sp.profile_id,
                    sp.auto_matched
                ])?;
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
            "SELECT s.id, s.label,
                    coalesce(s.display_name, CASE WHEN s.profile_id IS NULL THEN s.profile_name END),
                    s.is_me,
                    coalesce(sum(u.end_ms - u.start_ms), 0), count(u.id),
                    s.profile_id, p.name, s.auto_matched, s.embedding IS NOT NULL
             FROM speakers s
             LEFT JOIN utterances u ON u.speaker_id = s.id
             LEFT JOIN speaker_profiles p ON p.id = s.profile_id
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
                    profile_id: r.get(6)?,
                    profile_name: r.get(7)?,
                    auto_matched: r.get::<_, i64>(8)? != 0,
                    has_voice: r.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Sets (or with `None`, clears) the name shown for a speaker.
    pub fn rename_speaker(&self, speaker_id: i64, name: Option<&str>) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE speakers SET display_name = ?2, profile_name = NULL WHERE id = ?1",
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
            "SELECT s.id, coalesce(s.display_name, s.profile_name), u.start_ms, u.end_ms,
                    CASE WHEN s.auto_matched = 0 THEN s.profile_id END
             FROM speakers s JOIN utterances u ON u.speaker_id = s.id
             WHERE s.meeting_id = ?1 AND s.is_me = 0
               AND (s.display_name IS NOT NULL OR s.profile_name IS NOT NULL
                    OR (s.profile_id IS NOT NULL AND s.auto_matched = 0))
             ORDER BY s.id, u.start_ms",
        )?;
        let mut out: Vec<(i64, crate::speakers::OldSpeaker)> = Vec::new();
        let rows = stmt.query_map([meeting_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, name, s, e, profile_id) = row?;
            match out.last_mut() {
                Some((last, o)) if *last == id => o.spans.push((s, e)),
                _ => out.push((
                    id,
                    crate::speakers::OldSpeaker {
                        name,
                        profile_id,
                        spans: vec![(s, e)],
                    },
                )),
            }
        }
        Ok(out.into_iter().map(|(_, o)| o).collect())
    }

    // ---- Voice profiles ----

    /// Each profile's averaged voice for one embedding model (profiles without samples for that
    /// model are left out).
    pub fn profile_centroids(&self, model: &str) -> Result<Vec<(i64, Vec<f32>)>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT profile_id, vector FROM profile_embeddings WHERE model_id = ?1 ORDER BY profile_id",
        )?;
        let mut grouped: Vec<(i64, Vec<Vec<f32>>)> = Vec::new();
        let rows = stmt.query_map([model], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (id, blob) = row?;
            let v = crate::profiles::from_blob(&blob);
            match grouped.last_mut() {
                Some((last, vs)) if *last == id => vs.push(v),
                _ => grouped.push((id, vec![v])),
            }
        }
        Ok(grouped
            .into_iter()
            .filter_map(|(id, vs)| crate::profiles::centroid(&vs).map(|c| (id, c)))
            .collect())
    }

    /// Creates a profile from a meeting speaker's voice and links the speaker to it.
    pub fn create_profile_from_speaker(&self, speaker_id: i64, name: &str) -> Result<i64, DbError> {
        let now = now_ms();
        let id = {
            let c = self.conn();
            c.execute(
                "INSERT INTO speaker_profiles (name, created_at, updated_at) VALUES (?1, ?2, ?2)",
                params![name, now],
            )?;
            c.last_insert_rowid()
        };
        if let Err(e) = self.link_speaker_to_profile(speaker_id, id) {
            let _ = self
                .conn()
                .execute("DELETE FROM speaker_profiles WHERE id = ?1", [id]);
            return Err(e);
        }
        Ok(id)
    }

    /// Links a meeting speaker to a profile as confirmed, and adds the speaker's voice to the
    /// profile (newest [`crate::profiles::MAX_SAMPLES`] per model are kept).
    pub fn link_speaker_to_profile(&self, speaker_id: i64, profile_id: i64) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let (meeting_id, model, blob, speech_ms, is_me): (i64, Option<String>, Option<Vec<u8>>, i64, bool) = tx
            .query_row(
                "SELECT meeting_id, embedding_model, embedding, speech_ms, is_me FROM speakers WHERE id = ?1",
                [speaker_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0)),
            )
            .optional()?
            .ok_or(DbError::Invalid("that speaker doesn't exist"))?;
        if is_me {
            return Err(DbError::Invalid(
                "your own microphone can't have a voice profile",
            ));
        }
        let (Some(model), Some(blob)) = (model, blob) else {
            return Err(DbError::Invalid(
                "there isn't enough clear speech from this speaker to save their voice",
            ));
        };
        // One speaker per profile in a meeting.
        tx.execute(
            "UPDATE speakers SET profile_id = NULL, auto_matched = 0
             WHERE meeting_id = ?1 AND profile_id = ?2 AND id != ?3",
            params![meeting_id, profile_id, speaker_id],
        )?;
        tx.execute(
            "UPDATE speakers SET profile_id = ?2, auto_matched = 0, profile_name = NULL WHERE id = ?1",
            params![speaker_id, profile_id],
        )?;
        // Replace any earlier sample from this meeting, then keep the newest few.
        tx.execute(
            "DELETE FROM profile_embeddings WHERE profile_id = ?1 AND model_id = ?2 AND source_meeting_id = ?3",
            params![profile_id, model, meeting_id],
        )?;
        tx.execute(
            "INSERT INTO profile_embeddings (profile_id, model_id, vector, source_meeting_id, created_at, speech_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![profile_id, model, blob, meeting_id, now_ms(), speech_ms],
        )?;
        tx.execute(
            "DELETE FROM profile_embeddings WHERE profile_id = ?1 AND model_id = ?2 AND id NOT IN (
                 SELECT id FROM profile_embeddings WHERE profile_id = ?1 AND model_id = ?2
                 ORDER BY created_at DESC, id DESC LIMIT ?3)",
            params![profile_id, model, crate::profiles::MAX_SAMPLES as i64],
        )?;
        tx.execute(
            "UPDATE speaker_profiles SET updated_at = ?2 WHERE id = ?1",
            params![profile_id, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rejects a suggested match: the speaker goes back to "Speaker N".
    pub fn unlink_speaker(&self, speaker_id: i64) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE speakers SET profile_id = NULL, auto_matched = 0, profile_name = NULL WHERE id = ?1",
            [speaker_id],
        )?;
        Ok(())
    }

    pub fn profiles(&self) -> Result<Vec<ProfileRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT p.id, p.name, p.created_at,
                    (SELECT count(DISTINCT s.meeting_id) FROM speakers s WHERE s.profile_id = p.id),
                    (SELECT max(m.started_at) FROM speakers s JOIN meetings m ON m.id = s.meeting_id
                     WHERE s.profile_id = p.id),
                    (SELECT json_group_array(json_array(model_id, n)) FROM
                        (SELECT model_id, count(*) AS n FROM profile_embeddings e
                         WHERE e.profile_id = p.id GROUP BY model_id))
             FROM speaker_profiles p ORDER BY lower(p.name)",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let samples: Option<String> = r.get(5)?;
                Ok(ProfileRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                    meetings: r.get(3)?,
                    last_seen: r.get(4)?,
                    samples: samples
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn profile_meetings(&self, profile_id: i64) -> Result<Vec<ProfileMeeting>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT m.id, m.title, m.started_at, min(s.auto_matched)
             FROM speakers s JOIN meetings m ON m.id = s.meeting_id
             WHERE s.profile_id = ?1
             GROUP BY m.id ORDER BY m.started_at DESC",
        )?;
        let rows = stmt
            .query_map([profile_id], |r| {
                Ok(ProfileMeeting {
                    meeting_id: r.get(0)?,
                    title: r.get(1)?,
                    started_at: r.get(2)?,
                    auto_matched: r.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn rename_profile(&self, profile_id: i64, name: &str) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE speaker_profiles SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![profile_id, name, now_ms()],
        )?;
        Ok(())
    }

    /// Deletes a profile and its voice samples; meeting speakers linked to it keep their own names.
    pub fn delete_profile(&self, profile_id: i64) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE speakers SET auto_matched = 0 WHERE profile_id = ?1",
            [profile_id],
        )?;
        // Speakers' profile_id is cleared by ON DELETE SET NULL; samples go by CASCADE.
        tx.execute("DELETE FROM speaker_profiles WHERE id = ?1", [profile_id])?;
        tx.commit()?;
        Ok(())
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
                    u.speaker_id, s.label, coalesce(s.display_name, p.name, s.profile_name),
                    coalesce(s.is_me, 0), u.edited,
                    s.display_name IS NULL AND coalesce(s.auto_matched, 0) != 0
             FROM utterances u
             LEFT JOIN speakers s ON s.id = u.speaker_id
             LEFT JOIN speaker_profiles p ON p.id = s.profile_id
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
                    speaker_suggested: r.get::<_, Option<bool>>(10)?.unwrap_or(false),
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
            "UPDATE meetings SET status = 'incomplete', updated_at = ?1
             WHERE status = 'recording' AND device IS NULL",
            [now_ms()],
        )?)
    }

    pub fn list_meetings(&self) -> Result<Vec<MeetingRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT m.id, m.title, m.started_at, m.duration_ms, m.status, m.error,
                    m.remote_speaker_count_hint, m.tags, m.language,
                    (SELECT json_group_array(name) FROM (
                        SELECT coalesce(s.display_name, p.name, s.profile_name, s.label) AS name
                        FROM speakers s LEFT JOIN utterances u ON u.speaker_id = s.id
                        LEFT JOIN speaker_profiles p ON p.id = s.profile_id AND s.auto_matched = 0
                        WHERE s.meeting_id = m.id AND s.is_me = 0
                        GROUP BY s.id
                        ORDER BY coalesce(sum(u.end_ms - u.start_ms), 0) DESC)),
                    m.device IS NOT NULL
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
                elsewhere: r.get(10)?,
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

/// A summary template: what to focus on and which sections to write.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateRow {
    pub id: i64,
    pub name: String,
    pub instructions: String,
    pub sections: Vec<String>,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRow {
    pub markdown: String,
    pub template_id: Option<i64>,
    pub template_name: String,
    pub model_id: String,
    pub created_at: i64,
    pub edited: bool,
}

/// Built-in templates: (key, name, instructions, sections).
pub const BUILTIN_TEMPLATES: &[(&str, &str, &str, &[&str])] = &[
    (
        "general",
        "General meeting",
        "Capture what was discussed and agreed, and who will do what next.",
        &["Summary", "Decisions", "Action items", "Open questions"],
    ),
    (
        "client",
        "Client call",
        "Focus on the client's needs, concerns and requests, what was promised to them, and the \
         next steps on each side.",
        &[
            "Summary",
            "Client needs",
            "Commitments",
            "Next steps",
            "Open questions",
        ],
    ),
    (
        "interview",
        "Interview",
        "Summarize the candidate's background, the main points of their answers, strengths and \
         concerns raised, and any follow-ups. Stay neutral; don't make a hiring recommendation.",
        &[
            "Summary",
            "Background",
            "Key answers",
            "Strengths",
            "Concerns",
            "Follow-ups",
        ],
    ),
    (
        "one_on_one",
        "1:1",
        "Capture updates, wins, blockers, feedback given in either direction, and agreed next \
         steps.",
        &["Updates", "Blockers", "Feedback", "Action items"],
    ),
    (
        "brainstorm",
        "Brainstorm",
        "List the ideas raised, grouped by theme, which ones got the most support, and what will \
         be explored next.",
        &["Summary", "Ideas", "Favourites", "Next steps"],
    ),
];

/// Keep in step with the error message in `clean_template`.
const MAX_SECTIONS: usize = 12;

fn clean_template(
    name: &str,
    instructions: &str,
    sections: &[String],
) -> Result<(String, String, String), DbError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(DbError::Invalid(
            "a template needs a name of up to 60 characters",
        ));
    }
    let instructions = instructions.trim();
    if instructions.chars().count() > 2000 {
        return Err(DbError::Invalid(
            "keep the instructions under 2,000 characters",
        ));
    }
    let sections: Vec<&str> = sections
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if sections.len() > MAX_SECTIONS || sections.iter().any(|s| s.chars().count() > 60) {
        return Err(DbError::Invalid(
            "use up to 12 sections of up to 60 characters each",
        ));
    }
    Ok((
        name.to_owned(),
        instructions.to_owned(),
        serde_json::to_string(&sections).expect("serializes"),
    ))
}

impl Db {
    /// Adds any built-in template that isn't there yet (first run, or a new one in an update).
    pub fn ensure_builtin_templates(&self) -> Result<(), DbError> {
        let c = self.conn();
        for (i, (key, name, instructions, sections)) in BUILTIN_TEMPLATES.iter().enumerate() {
            c.execute(
                "INSERT OR IGNORE INTO templates
                     (name, system_prompt, sections_json, is_builtin, builtin_key, position)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![
                    name,
                    instructions,
                    serde_json::to_string(sections).expect("serializes"),
                    key,
                    i as i64
                ],
            )?;
        }
        Ok(())
    }

    fn template_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TemplateRow> {
        let sections: String = r.get(3)?;
        Ok(TemplateRow {
            id: r.get(0)?,
            name: r.get(1)?,
            instructions: r.get(2)?,
            sections: serde_json::from_str(&sections).unwrap_or_default(),
            builtin: r.get(4)?,
        })
    }

    pub fn templates(&self) -> Result<Vec<TemplateRow>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, name, system_prompt, sections_json, is_builtin FROM templates
             ORDER BY is_builtin DESC, position, id",
        )?;
        let rows = stmt.query_map([], Self::template_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn template(&self, id: i64) -> Result<Option<TemplateRow>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, name, system_prompt, sections_json, is_builtin FROM templates
                 WHERE id = ?1",
                [id],
                Self::template_from_row,
            )
            .optional()?)
    }

    /// Creates a template (`id` = `None`) or updates one; returns its id.
    pub fn save_template(
        &self,
        id: Option<i64>,
        name: &str,
        instructions: &str,
        sections: &[String],
    ) -> Result<i64, DbError> {
        let (name, instructions, sections) = clean_template(name, instructions, sections)?;
        let c = self.conn();
        match id {
            Some(id) => {
                let n = c.execute(
                    "UPDATE templates SET name = ?2, system_prompt = ?3, sections_json = ?4
                     WHERE id = ?1",
                    params![id, name, instructions, sections],
                )?;
                if n == 0 {
                    return Err(DbError::Invalid("that template no longer exists"));
                }
                Ok(id)
            }
            None => {
                c.execute(
                    "INSERT INTO templates (name, system_prompt, sections_json, is_builtin, position)
                     VALUES (?1, ?2, ?3, 0, (SELECT coalesce(max(position), 0) + 1 FROM templates))",
                    params![name, instructions, sections],
                )?;
                Ok(c.last_insert_rowid())
            }
        }
    }

    /// Deletes a template of the user's own (built-in ones can only be reset).
    pub fn delete_template(&self, id: i64) -> Result<(), DbError> {
        let n = self.conn().execute(
            "DELETE FROM templates WHERE id = ?1 AND is_builtin = 0",
            [id],
        )?;
        if n == 0 {
            return Err(DbError::Invalid("built-in templates can't be deleted"));
        }
        Ok(())
    }

    /// Restores a built-in template's original wording.
    pub fn reset_template(&self, id: i64) -> Result<(), DbError> {
        let c = self.conn();
        let key: Option<String> = c
            .query_row(
                "SELECT builtin_key FROM templates WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some((_, name, instructions, sections)) = key
            .as_deref()
            .and_then(|k| BUILTIN_TEMPLATES.iter().find(|t| t.0 == k))
        else {
            return Err(DbError::Invalid("only built-in templates can be reset"));
        };
        c.execute(
            "UPDATE templates SET name = ?2, system_prompt = ?3, sections_json = ?4 WHERE id = ?1",
            params![
                id,
                name,
                instructions,
                serde_json::to_string(sections).expect("serializes")
            ],
        )?;
        Ok(())
    }

    pub fn summary(&self, meeting_id: i64) -> Result<Option<SummaryRow>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT markdown, template_id, template_name, model_id, created_at, edited
                 FROM summaries WHERE meeting_id = ?1",
                [meeting_id],
                |r| {
                    Ok(SummaryRow {
                        markdown: r.get(0)?,
                        template_id: r.get(1)?,
                        template_name: r.get(2)?,
                        model_id: r.get(3)?,
                        created_at: r.get(4)?,
                        edited: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Stores a freshly written summary, replacing the meeting's previous one.
    pub fn save_summary(
        &self,
        meeting_id: i64,
        template: &TemplateRow,
        model_id: &str,
        markdown: &str,
    ) -> Result<(), DbError> {
        let now = now_ms();
        self.conn().execute(
            "INSERT INTO summaries (meeting_id, template_id, template_name, model_id, markdown,
                                    created_at, updated_at, edited)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 0)
             ON CONFLICT (meeting_id) DO UPDATE SET template_id = ?2, template_name = ?3,
                 model_id = ?4, markdown = ?5, created_at = ?6, updated_at = ?6, edited = 0",
            params![
                meeting_id,
                template.id,
                template.name,
                model_id,
                markdown,
                now
            ],
        )?;
        Ok(())
    }

    /// The user's own edit of a summary.
    pub fn edit_summary(&self, meeting_id: i64, markdown: &str) -> Result<(), DbError> {
        let n = self.conn().execute(
            "UPDATE summaries SET markdown = ?2, edited = 1, updated_at = ?3 WHERE meeting_id = ?1",
            params![meeting_id, markdown, now_ms()],
        )?;
        if n == 0 {
            return Err(DbError::Invalid("this meeting has no summary"));
        }
        Ok(())
    }

    /// Whether the meeting has no title of the user's own (empty, or one written by the model).
    pub fn title_is_open(&self, meeting_id: i64) -> Result<bool, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT trim(title) = '' OR title_generated FROM meetings WHERE id = ?1",
                [meeting_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Sets a model-written title, unless the user has given the meeting one meanwhile.
    pub fn set_generated_title(&self, meeting_id: i64, title: &str) -> Result<bool, DbError> {
        Ok(self.conn().execute(
            "UPDATE meetings SET title = ?2, title_generated = 1, updated_at = ?3
             WHERE id = ?1 AND (trim(title) = '' OR title_generated)",
            params![meeting_id, title, now_ms()],
        )? > 0)
    }
}

/// What the library needs to know about a meeting to save or read its folder.
#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub id: i64,
    pub uid: String,
    pub folder: Option<String>,
    pub status: String,
    pub file_hash: Option<String>,
    pub file_stamp: Option<String>,
}

// ---- Meeting folders (see library.rs) ----
impl Db {
    /// This computer's id, made on first use.
    pub fn device_id(&self) -> String {
        self.device
            .get_or_init(|| {
                if let Ok(Some(id)) = self.get_setting("device.id")
                    && !id.is_empty()
                {
                    return id;
                }
                let id = crate::library::new_uid();
                if let Err(e) = self.set_setting("device.id", &id) {
                    log::warn!("couldn't save this computer's id: {e}");
                }
                id
            })
            .clone()
    }

    /// Gives meetings from before 1.2 an id and their folder (where their audio is), and marks
    /// any meeting that was never saved to its folder to be saved.
    pub fn prepare_library(&self) -> Result<(), DbError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let missing: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM meetings WHERE uid IS NULL")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for id in missing {
            tx.execute(
                "UPDATE meetings SET uid = ?2 WHERE id = ?1",
                params![id, crate::library::new_uid()],
            )?;
        }
        let no_folder: Vec<(i64, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT m.id, (SELECT t.path FROM tracks t WHERE t.meeting_id = m.id ORDER BY t.id LIMIT 1)
                 FROM meetings m WHERE m.folder IS NULL",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        for (id, track) in no_folder {
            if let Some(dir) = track.as_deref().and_then(|t| Path::new(t).parent()) {
                tx.execute(
                    "UPDATE meetings SET folder = ?2 WHERE id = ?1",
                    params![id, dir.display().to_string()],
                )?;
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO library_dirty
                 SELECT id, 0 FROM meetings WHERE file_hash IS NULL AND folder IS NOT NULL",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn meeting_uid(&self, id: i64) -> Result<String, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT uid FROM meetings WHERE id = ?1", [id], |r| r.get(0))?)
    }

    pub fn set_meeting_folder(&self, id: i64, folder: &Path) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE meetings SET folder = ?2 WHERE id = ?1",
            params![id, folder.display().to_string()],
        )?;
        Ok(())
    }

    pub fn meeting_folder(&self, id: i64) -> Result<Option<String>, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT folder FROM meetings WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    /// Meetings changed since they were saved, with when.
    pub fn library_dirty(&self) -> Result<Vec<(i64, i64)>, DbError> {
        let c = self.conn();
        let mut stmt =
            c.prepare("SELECT meeting_id, changed_at FROM library_dirty ORDER BY changed_at")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Done saving a change; a newer change since `changed_at` stays queued.
    pub fn clear_library_dirty(&self, id: i64, changed_at: i64) -> Result<(), DbError> {
        self.conn().execute(
            "DELETE FROM library_dirty WHERE meeting_id = ?1 AND changed_at <= ?2",
            params![id, changed_at],
        )?;
        Ok(())
    }

    fn library_entry_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEntry> {
        Ok(LibraryEntry {
            id: r.get(0)?,
            uid: r.get(1)?,
            folder: r.get(2)?,
            status: r.get(3)?,
            file_hash: r.get(4)?,
            file_stamp: r.get(5)?,
        })
    }

    pub fn library_entry(&self, id: i64) -> Result<Option<LibraryEntry>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, uid, folder, status, file_hash, file_stamp FROM meetings
                 WHERE id = ?1 AND uid IS NOT NULL",
                [id],
                Self::library_entry_from_row,
            )
            .optional()?)
    }

    pub fn library_entries(&self) -> Result<Vec<LibraryEntry>, DbError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "SELECT id, uid, folder, status, file_hash, file_stamp FROM meetings
             WHERE uid IS NOT NULL",
        )?;
        let rows = stmt.query_map([], Self::library_entry_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records what the meeting's file holds now (written or read here).
    pub fn set_library_file(&self, id: i64, hash: &str, stamp: &str) -> Result<(), DbError> {
        self.conn().execute(
            "UPDATE meetings SET file_hash = ?2, file_stamp = ?3 WHERE id = ?1",
            params![id, hash, stamp],
        )?;
        Ok(())
    }

    /// The meeting's file is gone from its folder: save it again.
    pub fn forget_library_file(&self, id: i64) -> Result<(), DbError> {
        let c = self.conn();
        c.execute(
            "UPDATE meetings SET file_hash = NULL, file_stamp = NULL WHERE id = ?1",
            [id],
        )?;
        c.execute("INSERT OR IGNORE INTO library_dirty VALUES (?1, 0)", [id])?;
        Ok(())
    }

    pub fn has_active_job(&self, meeting_id: i64) -> Result<bool, DbError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE meeting_id = ?1 AND status IN ('queued', 'running'))",
            [meeting_id],
            |r| r.get(0),
        )?)
    }

    /// Everything about a meeting, as saved in its folder.
    pub fn meeting_file(&self, id: i64) -> Result<Option<crate::library::MeetingFile>, DbError> {
        use crate::library::{FileSpeaker, FileSummary, FileTrack, FileUtterance, FileVoice};
        let device = self.device_id();
        let c = self.conn();
        let Some(mut file) = c
            .query_row(
                "SELECT uid, coalesce(device, ?2), title, title_generated, started_at, duration_ms,
                        status, error, language, remote_speaker_count_hint, tags
                 FROM meetings WHERE id = ?1 AND uid IS NOT NULL",
                params![id, device],
                |r| {
                    let tags: String = r.get(10)?;
                    Ok(crate::library::MeetingFile {
                        format: crate::library::FORMAT,
                        uid: r.get(0)?,
                        device: r.get(1)?,
                        saved_at: 0,
                        title: r.get(2)?,
                        title_generated: r.get(3)?,
                        started_at: r.get(4)?,
                        duration_ms: r.get(5)?,
                        status: r.get(6)?,
                        error: r.get(7)?,
                        language: r.get(8)?,
                        speaker_count_hint: r.get(9)?,
                        tags: serde_json::from_str(&tags).unwrap_or_default(),
                        tracks: Vec::new(),
                        speakers: Vec::new(),
                        utterances: Vec::new(),
                        summary: None,
                    })
                },
            )
            .optional()?
        else {
            return Ok(None);
        };

        let mut stmt = c.prepare(
            "SELECT kind, path, sample_rate, channels, duration_ms, finalized, start_offset_ms,
                    source, source_spec
             FROM tracks WHERE meeting_id = ?1 ORDER BY kind",
        )?;
        file.tracks = stmt
            .query_map([id], |r| {
                let path: String = r.get(1)?;
                Ok(FileTrack {
                    kind: r.get(0)?,
                    file: Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(path),
                    sample_rate: r.get(2)?,
                    channels: r.get(3)?,
                    duration_ms: r.get(4)?,
                    finalized: r.get(5)?,
                    start_offset_ms: r.get(6)?,
                    source: r.get(7)?,
                    source_spec: r.get(8)?,
                })
            })?
            .collect::<Result<_, _>>()?;

        let mut stmt = c.prepare(
            "SELECT s.id, s.label, s.display_name, s.is_me, s.color, coalesce(p.name, s.profile_name),
                    s.auto_matched AND p.id IS NOT NULL, s.embedding_model, s.embedding, s.speech_ms
             FROM speakers s LEFT JOIN speaker_profiles p ON p.id = s.profile_id
             WHERE s.meeting_id = ?1 ORDER BY s.id",
        )?;
        let speakers: Vec<(i64, FileSpeaker)> = stmt
            .query_map([id], |r| {
                let model: Option<String> = r.get(7)?;
                let blob: Option<Vec<u8>> = r.get(8)?;
                Ok((
                    r.get(0)?,
                    FileSpeaker {
                        label: r.get(1)?,
                        name: r.get(2)?,
                        is_me: r.get(3)?,
                        color: r.get(4)?,
                        profile: r.get(5)?,
                        auto_matched: r.get(6)?,
                        voice: match (model, blob) {
                            (Some(model), Some(blob)) => Some(FileVoice {
                                model,
                                vector: crate::profiles::from_blob(&blob),
                                speech_ms: r.get(9)?,
                            }),
                            _ => None,
                        },
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let index: HashMap<i64, usize> = speakers
            .iter()
            .enumerate()
            .map(|(i, (sid, _))| (*sid, i))
            .collect();
        file.speakers = speakers.into_iter().map(|(_, s)| s).collect();

        let mut stmt = c.prepare(
            "SELECT id, track_kind, speaker_id, start_ms, end_ms, text, edited
             FROM utterances WHERE meeting_id = ?1 ORDER BY start_ms, id",
        )?;
        let mut words = c.prepare(
            "SELECT start_ms, end_ms, text, prob FROM words WHERE utterance_id = ?1
             ORDER BY start_ms, id",
        )?;
        let rows: Vec<(i64, FileUtterance)> = stmt
            .query_map([id], |r| {
                let speaker: Option<i64> = r.get(2)?;
                Ok((
                    r.get(0)?,
                    FileUtterance {
                        track: r.get(1)?,
                        speaker: speaker.and_then(|s| index.get(&s).copied()),
                        start_ms: r.get(3)?,
                        end_ms: r.get(4)?,
                        text: r.get(5)?,
                        edited: r.get(6)?,
                        words: Vec::new(),
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (uid, mut u) in rows {
            u.words = words
                .query_map([uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            file.utterances.push(u);
        }

        file.summary = c
            .query_row(
                "SELECT markdown, template_name, model_id, created_at, updated_at, edited
                 FROM summaries WHERE meeting_id = ?1",
                [id],
                |r| {
                    Ok(FileSummary {
                        markdown: r.get(0)?,
                        template: r.get(1)?,
                        model: r.get(2)?,
                        created_at: r.get(3)?,
                        updated_at: r.get(4)?,
                        edited: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(Some(file))
    }

    /// Adds or replaces a meeting from its folder's file (matched by its id); voice profiles are
    /// matched by name. Returns the meeting's id here.
    pub fn import_meeting_file(
        &self,
        f: &crate::library::MeetingFile,
        dir: &Path,
        hash: &str,
        stamp: Option<&str>,
    ) -> Result<i64, DbError> {
        let here = self.device_id();
        let device = f.device.as_deref().filter(|d| *d != here);
        // Only the computer that made a recording can continue it.
        let status = match f.status.as_str() {
            "recording" => "incomplete",
            s @ ("incomplete" | "queued" | "processing" | "ready" | "failed") => s,
            _ => "failed",
        };
        let tags = serde_json::to_string(&f.tags).unwrap_or_else(|_| "[]".into());
        let folder = dir.display().to_string();
        let now = now_ms();
        let mut c = self.conn();
        let tx = c.transaction()?;
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM meetings WHERE uid = ?1", [&f.uid], |r| {
                r.get(0)
            })
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE meetings SET title = ?2, title_generated = ?3, started_at = ?4,
                         duration_ms = ?5, status = ?6, error = ?7, language = ?8,
                         remote_speaker_count_hint = ?9, tags = ?10, updated_at = ?11
                     WHERE id = ?1",
                    params![
                        id,
                        f.title,
                        f.title_generated,
                        f.started_at,
                        f.duration_ms,
                        status,
                        f.error,
                        f.language,
                        f.speaker_count_hint,
                        tags,
                        now
                    ],
                )?;
                tx.execute("DELETE FROM tracks WHERE meeting_id = ?1", [id])?;
                tx.execute("DELETE FROM utterances WHERE meeting_id = ?1", [id])?;
                tx.execute("DELETE FROM speakers WHERE meeting_id = ?1", [id])?;
                tx.execute("DELETE FROM summaries WHERE meeting_id = ?1", [id])?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO meetings (uid, title, title_generated, started_at, duration_ms,
                         status, error, language, remote_speaker_count_hint, tags, created_at,
                         updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        f.uid,
                        f.title,
                        f.title_generated,
                        f.started_at,
                        f.duration_ms,
                        status,
                        f.error,
                        f.language,
                        f.speaker_count_hint,
                        tags,
                        now
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "UPDATE meetings SET folder = ?2, device = ?3, file_hash = ?4, file_stamp = ?5
             WHERE id = ?1",
            params![id, folder, device, hash, stamp],
        )?;

        let mut kinds = std::collections::HashSet::new();
        for t in &f.tracks {
            // Plain file names in the meeting's own folder only, one track per kind.
            let plain = Path::new(&t.file).file_name().and_then(|n| n.to_str()) == Some(&t.file);
            if !plain
                || !matches!(t.kind.as_str(), "mic" | "system")
                || !kinds.insert(t.kind.as_str())
            {
                continue;
            }
            tx.execute(
                "INSERT INTO tracks (meeting_id, kind, path, sample_rate, channels, duration_ms,
                     finalized, start_offset_ms, source, source_spec)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    t.kind,
                    dir.join(&t.file).display().to_string(),
                    t.sample_rate,
                    t.channels,
                    t.duration_ms,
                    t.finalized,
                    t.start_offset_ms,
                    t.source,
                    t.source_spec
                ],
            )?;
        }

        let mut speaker_ids = Vec::with_capacity(f.speakers.len());
        for sp in &f.speakers {
            let profile_id: Option<i64> = match &sp.profile {
                Some(name) if !sp.is_me => tx
                    .query_row(
                        "SELECT id FROM speaker_profiles WHERE lower(name) = lower(?1)
                         ORDER BY id LIMIT 1",
                        [name],
                        |r| r.get(0),
                    )
                    .optional()?,
                _ => None,
            };
            // A confirmed profile this computer doesn't have still names the speaker.
            let profile_name = sp
                .profile
                .as_ref()
                .filter(|_| profile_id.is_none() && !sp.auto_matched && !sp.is_me);
            let (model, blob) = match &sp.voice {
                Some(v) => (
                    Some(v.model.as_str()),
                    Some(crate::profiles::to_blob(&v.vector)),
                ),
                None => (None, None),
            };
            tx.execute(
                "INSERT INTO speakers (meeting_id, label, display_name, profile_id, is_me,
                     auto_matched, color, embedding, embedding_model, speech_ms, profile_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id,
                    sp.label,
                    sp.name,
                    profile_id,
                    sp.is_me,
                    sp.auto_matched && profile_id.is_some(),
                    sp.color,
                    blob,
                    model,
                    sp.voice.as_ref().map_or(0, |v| v.speech_ms),
                    profile_name
                ],
            )?;
            speaker_ids.push(tx.last_insert_rowid());
        }

        {
            let mut ins_u = tx.prepare(
                "INSERT INTO utterances (meeting_id, speaker_id, track_kind, start_ms, end_ms, text, edited)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut ins_w = tx.prepare(
                "INSERT INTO words (utterance_id, start_ms, end_ms, text, prob) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for u in &f.utterances {
                if !matches!(u.track.as_str(), "mic" | "system") {
                    continue;
                }
                let speaker = u.speaker.and_then(|i| speaker_ids.get(i).copied());
                ins_u.execute(params![
                    id, speaker, u.track, u.start_ms, u.end_ms, u.text, u.edited
                ])?;
                let uid = tx.last_insert_rowid();
                for (s, e, text, prob) in &u.words {
                    ins_w.execute(params![uid, s, e, text, prob])?;
                }
            }
        }

        if let Some(s) = &f.summary {
            tx.execute(
                "INSERT INTO summaries (meeting_id, template_id, template_name, model_id, markdown,
                     created_at, updated_at, edited)
                 VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    s.template,
                    s.model,
                    s.markdown,
                    s.created_at,
                    s.updated_at,
                    s.edited
                ],
            )?;
        }
        // What was just read in is already saved.
        tx.execute("DELETE FROM library_dirty WHERE meeting_id = ?1", [id])?;
        tx.commit()?;
        Ok(id)
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
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
            },
            NewSpeaker {
                label: "Speaker 2".into(),
                is_me: false,
                name: None,
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
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
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
            },
            NewSpeaker {
                label: "Speaker 2".into(),
                is_me: false,
                name: None,
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
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
        assert_eq!(named[0].name.as_deref(), Some("Dana"));
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
    fn voice_profiles() {
        let db = Db::open_in_memory().unwrap();
        let voice = |x: f32| {
            Some((
                "cam".to_owned(),
                crate::profiles::normalize(vec![x, 1.0 - x, 0.1]),
            ))
        };
        let sp = |label: &str, is_me: bool, embedding: Option<(String, Vec<f32>)>| NewSpeaker {
            label: label.into(),
            is_me,
            name: None,
            speech_ms: if embedding.is_some() { 20_000 } else { 0 },
            embedding,
            profile_id: None,
            auto_matched: false,
        };
        let utt = |speaker: usize, s: i64| NewUtterance {
            speaker: Some(speaker),
            start_ms: s,
            end_ms: s + 1000,
            text: "hi".into(),
            words: Vec::new(),
        };
        let mut meetings = Vec::new();
        for i in 0..12 {
            let m = db.create_meeting(1_000 + i, "m").unwrap();
            db.save_transcript(
                m,
                &[
                    sp("Speaker 1", false, voice(0.9)),
                    sp("Speaker 2", false, None),
                    sp("Me", true, None),
                ],
                &[
                    ("system", &[utt(0, 0), utt(1, 2000)]),
                    ("mic", &[utt(2, 4000)]),
                ],
            )
            .unwrap();
            meetings.push(m);
        }
        let ids = |m| db.speakers(m).unwrap();
        let first = ids(meetings[0]);
        let (me, s1, s2) = (first[0].id, first[1].id, first[2].id);
        assert!(first[1].has_voice && !first[2].has_voice);

        // Saving needs a voice; the mic speaker can't have a profile.
        assert!(db.create_profile_from_speaker(me, "Me").is_err());
        assert!(db.create_profile_from_speaker(s2, "Nobody").is_err());
        assert!(
            db.profiles().unwrap().is_empty(),
            "failed saves leave nothing behind"
        );
        let dana = db.create_profile_from_speaker(s1, "Dana").unwrap();
        let row = &ids(meetings[0])[1];
        assert_eq!(
            (
                row.profile_id,
                row.profile_name.as_deref(),
                row.auto_matched
            ),
            (Some(dana), Some("Dana"), false)
        );
        // The profile's name stands in for "Speaker 1" in the transcript.
        assert_eq!(
            db.transcript(meetings[0]).unwrap()[0]
                .speaker_name
                .as_deref(),
            Some("Dana")
        );

        // Confirming in eleven more meetings keeps only the newest ten samples.
        for &m in &meetings[1..] {
            db.link_speaker_to_profile(ids(m)[1].id, dana).unwrap();
        }
        let p = &db.profiles().unwrap()[0];
        assert_eq!(
            (p.meetings, p.samples.clone()),
            (12, vec![("cam".to_owned(), 10)])
        );
        assert_eq!(db.profile_centroids("cam").unwrap().len(), 1);
        assert!(db.profile_centroids("other-model").unwrap().is_empty());
        assert_eq!(db.profile_meetings(dana).unwrap().len(), 12);

        // Confirmed links carry over a re-run; suggestions and rejections don't.
        let named = db.named_speakers(meetings[0]).unwrap();
        assert_eq!(
            (named[0].name.clone(), named[0].profile_id),
            (None, Some(dana))
        );
        db.unlink_speaker(ids(meetings[1])[1].id).unwrap();
        assert!(db.named_speakers(meetings[1]).unwrap().is_empty());

        db.rename_profile(dana, "Dana R.").unwrap();
        assert_eq!(ids(meetings[0])[1].profile_name.as_deref(), Some("Dana R."));
        db.delete_profile(dana).unwrap();
        assert!(db.profiles().unwrap().is_empty());
        assert_eq!(
            ids(meetings[0])[1].profile_id,
            None,
            "speakers are unlinked, not deleted"
        );
        assert!(db.profile_centroids("cam").unwrap().is_empty());
    }

    #[test]
    fn migrations_are_valid() {
        MIGRATIONS.validate().unwrap();
    }

    #[test]
    fn templates_are_seeded_edited_and_reset() {
        let db = Db::open_in_memory().unwrap();
        // Seeding again (every launch) adds nothing.
        db.ensure_builtin_templates().unwrap();
        let all = db.templates().unwrap();
        assert_eq!(all.len(), BUILTIN_TEMPLATES.len());
        assert_eq!(all[0].name, "General meeting");
        assert!(all.iter().all(|t| t.builtin));

        let general = all[0].id;
        db.save_template(
            Some(general),
            " Standup ",
            "Short.",
            &["Done".into(), " ".into()],
        )
        .unwrap();
        let t = db.template(general).unwrap().unwrap();
        assert_eq!(
            (t.name.as_str(), t.sections.as_slice()),
            ("Standup", &["Done".to_owned()][..])
        );
        assert!(
            db.delete_template(general).is_err(),
            "built-ins can't be deleted"
        );
        db.reset_template(general).unwrap();
        assert_eq!(
            db.template(general).unwrap().unwrap().name,
            "General meeting"
        );

        let mine = db
            .save_template(None, "Retro", "What went well.", &[])
            .unwrap();
        assert!(!db.template(mine).unwrap().unwrap().builtin);
        assert_eq!(db.templates().unwrap().last().unwrap().id, mine);
        assert!(db.reset_template(mine).is_err());
        assert!(db.save_template(None, "  ", "", &[]).is_err());
        db.delete_template(mine).unwrap();
        assert!(db.template(mine).unwrap().is_none());
    }

    #[test]
    fn summaries_replace_and_titles_respect_the_user() {
        let db = Db::open_in_memory().unwrap();
        let m = db.create_meeting(1_000, "").unwrap();
        let t = db.templates().unwrap().remove(0);
        assert!(db.summary(m).unwrap().is_none());
        assert!(db.edit_summary(m, "x").is_err());
        db.save_summary(m, &t, "qwen3.5-4b", "## Summary\nOne.")
            .unwrap();
        db.edit_summary(m, "## Summary\nEdited.").unwrap();
        assert!(db.summary(m).unwrap().unwrap().edited);
        db.save_summary(m, &t, "qwen3.5-9b", "## Summary\nTwo.")
            .unwrap();
        let s = db.summary(m).unwrap().unwrap();
        assert_eq!(
            (s.markdown.as_str(), s.model_id.as_str(), s.edited),
            ("## Summary\nTwo.", "qwen3.5-9b", false)
        );
        assert_eq!(s.template_name, "General meeting");

        // Untitled -> generated title, which a later one may replace.
        assert!(db.title_is_open(m).unwrap());
        assert!(db.set_generated_title(m, "Budget review").unwrap());
        assert!(db.title_is_open(m).unwrap());
        assert!(db.set_generated_title(m, "Q3 budget review").unwrap());
        // Keeping the generated title while editing other details keeps it replaceable...
        db.update_meeting_details(m, "Q3 budget review", None, &[])
            .unwrap();
        assert!(db.title_is_open(m).unwrap());
        // ...but a title the user typed is theirs.
        db.update_meeting_details(m, "Budget", None, &[]).unwrap();
        assert!(!db.title_is_open(m).unwrap());
        assert!(!db.set_generated_title(m, "Other").unwrap());
        assert_eq!(db.meeting(m).unwrap().unwrap().title, "Budget");

        // Deleting the meeting deletes its summary.
        db.delete_meeting(m).unwrap();
        assert!(db.summary(m).unwrap().is_none());
    }

    #[test]
    fn migrates_to_latest() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), 7);
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
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
            },
            NewSpeaker {
                label: "Me".into(),
                is_me: true,
                name: None,
                embedding: None,
                speech_ms: 0,
                profile_id: None,
                auto_matched: false,
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

    #[test]
    fn meetings_from_before_folders_get_an_id_and_their_folder() {
        let db = Db::open_in_memory().unwrap();
        let old = db.create_meeting(1_000, "Old").unwrap();
        let dir = Path::new("recordings").join(format!("meeting-{old}"));
        db.add_track(
            old,
            "mic",
            &dir.join("mic.flac").display().to_string(),
            48_000,
            1,
            "Mic",
            None,
        )
        .unwrap();
        let no_audio = db.create_meeting(2_000, "Failed").unwrap();
        // As a 1.1 database looks after the upgrade.
        db.conn()
            .execute_batch("UPDATE meetings SET uid = NULL; DELETE FROM library_dirty;")
            .unwrap();

        db.prepare_library().unwrap();
        let uid = db.meeting_uid(old).unwrap();
        assert_eq!(uid.len(), 32);
        assert_ne!(uid, db.meeting_uid(no_audio).unwrap());
        assert_eq!(
            db.meeting_folder(old).unwrap(),
            Some(dir.display().to_string())
        );
        assert_eq!(db.meeting_folder(no_audio).unwrap(), None);
        // Only the meeting with a folder is waiting to be saved.
        let dirty: Vec<i64> = db
            .library_dirty()
            .unwrap()
            .into_iter()
            .map(|d| d.0)
            .collect();
        assert_eq!(dirty, [old]);
        // Running it again changes nothing.
        db.prepare_library().unwrap();
        assert_eq!(db.meeting_uid(old).unwrap(), uid);
    }
}
