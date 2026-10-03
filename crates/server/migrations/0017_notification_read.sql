-- Read-state for in-app notifications. NULL = unread; Rfc3339 = when read.
ALTER TABLE reminders ADD COLUMN read_at TEXT;
CREATE INDEX idx_reminders_unread ON reminders(user_id, read_at);
