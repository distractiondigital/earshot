-- M1 recording: track alignment and source info, meeting tags.

-- Offset of a track's first sample from the meeting start (all tracks share the meeting clock).
ALTER TABLE tracks ADD COLUMN start_offset_ms INTEGER NOT NULL DEFAULT 0;
-- Human-readable source: device name, "All system audio" or an app name.
ALTER TABLE tracks ADD COLUMN source TEXT;
-- JSON array of tag strings.
ALTER TABLE meetings ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
