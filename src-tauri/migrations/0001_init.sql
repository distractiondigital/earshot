-- Initial schema. Timestamps are Unix epoch milliseconds (UTC).

CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE meetings (
    id                        INTEGER PRIMARY KEY,
    title                     TEXT NOT NULL DEFAULT '',
    started_at                INTEGER NOT NULL,
    duration_ms               INTEGER NOT NULL DEFAULT 0,
    status                    TEXT NOT NULL DEFAULT 'recording'
        CHECK (status IN ('recording', 'incomplete', 'queued', 'processing', 'ready', 'failed')),
    error                     TEXT,
    language                  TEXT,
    remote_speaker_count_hint INTEGER,
    created_at                INTEGER NOT NULL,
    updated_at                INTEGER NOT NULL
) STRICT;
CREATE INDEX meetings_started_at ON meetings (started_at DESC);

CREATE TABLE tracks (
    id          INTEGER PRIMARY KEY,
    meeting_id  INTEGER NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('mic', 'system')),
    path        TEXT NOT NULL,
    sample_rate INTEGER NOT NULL,
    channels    INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    finalized   INTEGER NOT NULL DEFAULT 0 CHECK (finalized IN (0, 1)),
    UNIQUE (meeting_id, kind)
) STRICT;

CREATE TABLE speaker_profiles (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE speakers (
    id           INTEGER PRIMARY KEY,
    meeting_id   INTEGER NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
    label        TEXT NOT NULL,
    display_name TEXT,
    profile_id   INTEGER REFERENCES speaker_profiles (id) ON DELETE SET NULL,
    is_me        INTEGER NOT NULL DEFAULT 0 CHECK (is_me IN (0, 1)),
    auto_matched INTEGER NOT NULL DEFAULT 0 CHECK (auto_matched IN (0, 1)),
    color        TEXT
) STRICT;
CREATE INDEX speakers_meeting ON speakers (meeting_id);
CREATE INDEX speakers_profile ON speakers (profile_id);

CREATE TABLE utterances (
    id         INTEGER PRIMARY KEY,
    meeting_id INTEGER NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
    speaker_id INTEGER REFERENCES speakers (id) ON DELETE SET NULL,
    track_kind TEXT NOT NULL CHECK (track_kind IN ('mic', 'system')),
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    text       TEXT NOT NULL,
    edited     INTEGER NOT NULL DEFAULT 0 CHECK (edited IN (0, 1))
) STRICT;
CREATE INDEX utterances_meeting_start ON utterances (meeting_id, start_ms);
CREATE INDEX utterances_speaker ON utterances (speaker_id);

CREATE TABLE words (
    id           INTEGER PRIMARY KEY,
    utterance_id INTEGER NOT NULL REFERENCES utterances (id) ON DELETE CASCADE,
    start_ms     INTEGER NOT NULL,
    end_ms       INTEGER NOT NULL,
    text         TEXT NOT NULL,
    prob         REAL
) STRICT;
CREATE INDEX words_utterance ON words (utterance_id, start_ms);

CREATE TABLE profile_embeddings (
    id                INTEGER PRIMARY KEY,
    profile_id        INTEGER NOT NULL REFERENCES speaker_profiles (id) ON DELETE CASCADE,
    model_id          TEXT NOT NULL,
    vector            BLOB NOT NULL,
    source_meeting_id INTEGER REFERENCES meetings (id) ON DELETE SET NULL
) STRICT;
CREATE INDEX profile_embeddings_profile ON profile_embeddings (profile_id, model_id);

CREATE TABLE templates (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    system_prompt TEXT NOT NULL,
    sections_json TEXT NOT NULL DEFAULT '[]',
    is_builtin    INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1))
) STRICT;

CREATE TABLE summaries (
    id          INTEGER PRIMARY KEY,
    meeting_id  INTEGER NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
    template_id INTEGER REFERENCES templates (id) ON DELETE SET NULL,
    model_id    TEXT NOT NULL,
    markdown    TEXT NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX summaries_meeting ON summaries (meeting_id);

-- Catalog entries live in the bundled models.json; this table tracks local state.
CREATE TABLE models (
    id         TEXT PRIMARY KEY NOT NULL,
    category   TEXT NOT NULL CHECK (category IN ('transcription', 'vad', 'segmentation', 'embedding', 'summary')),
    name       TEXT NOT NULL,
    file       TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'available'
        CHECK (state IN ('available', 'downloading', 'ready', 'error')),
    selected   INTEGER NOT NULL DEFAULT 0 CHECK (selected IN (0, 1))
) STRICT;

CREATE TABLE jobs (
    id         INTEGER PRIMARY KEY,
    meeting_id INTEGER REFERENCES meetings (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    stage      TEXT,
    progress   REAL NOT NULL DEFAULT 0,
    status     TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    error      TEXT,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX jobs_status ON jobs (status, created_at);

-- Full-text search over transcript text (external-content table kept in sync by triggers).
CREATE VIRTUAL TABLE utterances_fts USING fts5 (
    text,
    content = 'utterances',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER utterances_ai AFTER INSERT ON utterances BEGIN
    INSERT INTO utterances_fts (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER utterances_ad AFTER DELETE ON utterances BEGIN
    INSERT INTO utterances_fts (utterances_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER utterances_au AFTER UPDATE OF text ON utterances BEGIN
    INSERT INTO utterances_fts (utterances_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO utterances_fts (rowid, text) VALUES (new.id, new.text);
END;
