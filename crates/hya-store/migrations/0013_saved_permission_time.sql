-- Creation time of a saved "allow always" grant, milliseconds since the Unix
-- epoch. Rows saved before this migration keep NULL (unknown time).
ALTER TABLE saved_permission ADD COLUMN time_created INTEGER;
