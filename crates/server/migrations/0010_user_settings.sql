-- Per-user settings: currently just the user's preferred chat model.
--
-- One row per user (keyed by users.id). `chat_model` is the model id the user
-- selected from the deployment's allowed list; NULL means "use the config
-- default". Validated against the allowed list on write, and re-validated on
-- read so a model later removed from the allowed list falls back to the default.

CREATE TABLE user_settings (
    user_id    TEXT PRIMARY KEY,   -- users.id (OIDC sub)
    chat_model TEXT,               -- selected chat model id; NULL => config default
    updated_at TEXT NOT NULL       -- Rfc3339
);
