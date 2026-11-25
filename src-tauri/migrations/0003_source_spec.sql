-- Exact meeting-audio source of a track (JSON), so an interrupted meeting can reopen it.
ALTER TABLE tracks ADD COLUMN source_spec TEXT;
