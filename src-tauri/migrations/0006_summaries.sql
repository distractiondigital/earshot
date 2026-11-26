-- M6: summaries. One summary per meeting (regenerating replaces it); the template's name is kept
-- so a summary still says how it was made after its template is deleted. Built-in templates are
-- added at startup (see db.rs) and can be edited or reset; `builtin_key` identifies them.
ALTER TABLE summaries ADD COLUMN template_name TEXT NOT NULL DEFAULT '';
ALTER TABLE summaries ADD COLUMN edited INTEGER NOT NULL DEFAULT 0 CHECK (edited IN (0, 1));
ALTER TABLE summaries ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX summaries_one_per_meeting ON summaries (meeting_id);
DROP INDEX summaries_meeting;

ALTER TABLE templates ADD COLUMN builtin_key TEXT;
ALTER TABLE templates ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX templates_builtin ON templates (builtin_key) WHERE builtin_key IS NOT NULL;

-- Whether the meeting's title was written by the summary model (and may be replaced by a later one).
ALTER TABLE meetings ADD COLUMN title_generated INTEGER NOT NULL DEFAULT 0 CHECK (title_generated IN (0, 1));
