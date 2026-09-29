-- Notifications: scheduled reminders/tips/check-ins + push-notification tokens.
--
-- A `reminders` row is the unit of both scheduling and in-app delivery: the
-- scheduler polls rows with `sent_at IS NULL AND scheduled_at <= now`, delivers
-- them (logs today; push later), and stamps `sent_at`. A row with `sent_at` set
-- IS the delivered in-app notification returned by `GET /api/notifications`.

CREATE TABLE reminders (
    id           TEXT PRIMARY KEY,               -- uuid v4
    user_id      TEXT NOT NULL REFERENCES users(id),
    goal_id      TEXT,                            -- optional: goal this relates to
    step_id      TEXT,                            -- optional: roadmap step this relates to
    kind         TEXT NOT NULL,                   -- ReminderKind: reminder|tip|checkin
    scheduled_at TEXT NOT NULL,                   -- Rfc3339; when it becomes due
    sent_at      TEXT,                            -- Rfc3339; NULL until delivered
    channel      TEXT NOT NULL DEFAULT 'inapp',   -- inapp|push|...
    payload      TEXT NOT NULL DEFAULT '{}',      -- JSON blob (title/body/etc.)
    created_at   TEXT NOT NULL
);

-- List a user's notifications (GET /api/notifications).
CREATE INDEX idx_reminders_user ON reminders(user_id);

-- Scheduler poll: WHERE sent_at IS NULL AND scheduled_at <= now.
CREATE INDEX idx_reminders_due ON reminders(sent_at, scheduled_at);

CREATE TABLE push_tokens (
    id         TEXT PRIMARY KEY,                  -- uuid v4
    user_id    TEXT NOT NULL REFERENCES users(id),
    platform   TEXT NOT NULL,                     -- ios|android|web
    token      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(user_id, token)
);

CREATE INDEX idx_push_tokens_user ON push_tokens(user_id);
