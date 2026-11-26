-- M5: voice profiles. Each meeting speaker keeps the voice embedding computed during speaker
-- separation (model-specific), and profile samples record when they were added.
ALTER TABLE speakers ADD COLUMN embedding BLOB;
ALTER TABLE speakers ADD COLUMN embedding_model TEXT;
ALTER TABLE speakers ADD COLUMN speech_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE profile_embeddings ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE profile_embeddings ADD COLUMN speech_ms INTEGER NOT NULL DEFAULT 0;
