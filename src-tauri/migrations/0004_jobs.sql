-- M2 processing jobs: per-job options (e.g. forced CPU backend) and a last-update time.
ALTER TABLE jobs ADD COLUMN options TEXT;
ALTER TABLE jobs ADD COLUMN updated_at INTEGER;
