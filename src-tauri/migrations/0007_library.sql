-- 1.2: every meeting is also saved as meeting.json in its own folder, so a recordings folder in
-- cloud storage carries whole meetings between computers. This database stays the local index.
--   uid        identifies the meeting on every computer (local ids differ).
--   folder     the meeting's folder on this computer.
--   device     the computer that recorded it; NULL means this one.
--   file_hash  SHA-256 of meeting.json as last written or read here; NULL = never saved.
--   file_stamp size and modified time of that file, to skip unchanged files cheaply.
ALTER TABLE meetings ADD COLUMN uid TEXT;
ALTER TABLE meetings ADD COLUMN folder TEXT;
ALTER TABLE meetings ADD COLUMN device TEXT;
ALTER TABLE meetings ADD COLUMN file_hash TEXT;
ALTER TABLE meetings ADD COLUMN file_stamp TEXT;
CREATE UNIQUE INDEX meetings_uid ON meetings (uid) WHERE uid IS NOT NULL;

-- Meetings changed since their meeting.json was written, and when (filled by the triggers below,
-- which avoid conflict clauses: an upsert outside would override them; no foreign key, so rows
-- can be added while a meeting is being deleted).
CREATE TABLE library_dirty (
    meeting_id INTEGER PRIMARY KEY NOT NULL,
    changed_at INTEGER NOT NULL
) STRICT;

CREATE TRIGGER library_meeting_ai AFTER INSERT ON meetings BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.id;
    INSERT INTO library_dirty SELECT new.id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.id);
END;
CREATE TRIGGER library_meeting_au AFTER UPDATE OF title, title_generated, started_at, duration_ms,
    status, error, language, remote_speaker_count_hint, tags ON meetings BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.id;
    INSERT INTO library_dirty SELECT new.id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.id);
END;

CREATE TRIGGER library_tracks_ai AFTER INSERT ON tracks BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_tracks_au AFTER UPDATE ON tracks BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;

CREATE TRIGGER library_speakers_ai AFTER INSERT ON speakers BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_speakers_au AFTER UPDATE ON speakers BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_speakers_ad AFTER DELETE ON speakers BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = old.meeting_id;
    INSERT INTO library_dirty SELECT old.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = old.meeting_id);
END;

CREATE TRIGGER library_utterances_ai AFTER INSERT ON utterances BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_utterances_au AFTER UPDATE ON utterances BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_utterances_ad AFTER DELETE ON utterances BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = old.meeting_id;
    INSERT INTO library_dirty SELECT old.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = old.meeting_id);
END;

CREATE TRIGGER library_summaries_ai AFTER INSERT ON summaries BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_summaries_au AFTER UPDATE ON summaries BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = new.meeting_id;
    INSERT INTO library_dirty SELECT new.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = new.meeting_id);
END;
CREATE TRIGGER library_summaries_ad AFTER DELETE ON summaries BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER) WHERE meeting_id = old.meeting_id;
    INSERT INTO library_dirty SELECT old.meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE NOT EXISTS (SELECT 1 FROM library_dirty WHERE meeting_id = old.meeting_id);
END;

-- A renamed voice profile changes the names shown in the meetings it's linked to.
CREATE TRIGGER library_profiles_au AFTER UPDATE OF name ON speaker_profiles BEGIN
    UPDATE library_dirty SET changed_at = CAST(unixepoch('subsec') * 1000 AS INTEGER)
        WHERE meeting_id IN (SELECT meeting_id FROM speakers WHERE profile_id = new.id);
    INSERT INTO library_dirty SELECT DISTINCT meeting_id, CAST(unixepoch('subsec') * 1000 AS INTEGER) FROM speakers
        WHERE profile_id = new.id
          AND meeting_id NOT IN (SELECT meeting_id FROM library_dirty);
END;

-- Name of the voice profile a speaker was confirmed as on another computer, kept when this
-- computer has no profile of that name (so the name shows, and goes back out unchanged).
ALTER TABLE speakers ADD COLUMN profile_name TEXT;
